//! Multi-model routing engine with health tracking and fallback chains.
//!
//! Each Claude tier (opus/sonnet/haiku/default) can map to multiple
//! provider/model combinations. The router selects the next healthy
//! endpoint using the configured strategy and provides fallback
//! when an endpoint fails.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use std::time::Instant;

use tokio::sync::{Mutex, RwLock};
use tracing::{info, warn};

use crate::panel_config::{ProfileConfig, RoutingStrategy};

/// A single model endpoint (provider + model combination).
pub struct ModelEndpoint {
    pub provider: String,
    pub model_name: String,
    pub full_spec: String,
    pub healthy: AtomicBool,
    pub consecutive_errors: AtomicU32,
    pub cooldown_until: Mutex<Option<Instant>>,
}

/// Serializable status snapshot of a model endpoint for UI display.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ModelEndpointStatus {
    pub provider: String,
    pub model_name: String,
    pub full_spec: String,
    pub healthy: bool,
    pub on_cooldown: bool,
    pub consecutive_errors: u32,
}

/// Result of model resolution — contains the selected endpoint and tier info.
pub struct ResolvedModel {
    pub endpoint: Arc<ModelEndpoint>,
    #[allow(dead_code)]
    pub tier: String,
}

/// Router that manages model endpoints per Claude tier.
pub struct ModelRouter {
    tiers: RwLock<HashMap<String, Vec<Arc<ModelEndpoint>>>>,
    strategy: RwLock<RoutingStrategy>,
    round_robin_indices: RwLock<HashMap<String, AtomicUsize>>,
    cooldown_seconds: RwLock<u64>,
    max_consecutive_errors: RwLock<u32>,
}

impl ModelRouter {
    /// Build the router from a profile configuration.
    pub fn from_config(config: &ProfileConfig) -> Self {
        let tiers = build_tiers(&config.model_mapping);
        let indices: HashMap<String, AtomicUsize> = tiers
            .keys()
            .map(|k| (k.clone(), AtomicUsize::new(0)))
            .collect();

        Self {
            tiers: RwLock::new(tiers),
            strategy: RwLock::new(RoutingStrategy::parse(&config.routing.model_strategy)),
            round_robin_indices: RwLock::new(indices),
            cooldown_seconds: RwLock::new(config.routing.rate_limit_cooldown),
            max_consecutive_errors: RwLock::new(config.routing.max_consecutive_errors),
        }
    }

    /// Resolve a Claude model name to the next healthy model endpoint.
    pub async fn resolve(&self, claude_model: &str) -> Option<ResolvedModel> {
        let tier = detect_tier(claude_model);
        let tiers = self.tiers.read().await;

        let (resolved_tier, endpoints) = tiers
            .get_key_value(&tier)
            .or_else(|| tiers.get_key_value("default"))?;
        if endpoints.is_empty() {
            return None;
        }

        let strategy = *self.strategy.read().await;
        let endpoint = match strategy {
            RoutingStrategy::RoundRobin => self.select_round_robin(resolved_tier, endpoints).await,
            RoutingStrategy::Random => self.select_random(endpoints).await,
            RoutingStrategy::LeastErrors => self.select_least_errors(endpoints).await,
        }?;

        Some(ResolvedModel {
            endpoint,
            tier: tier.to_string(),
        })
    }

    /// Get the next fallback endpoint after a failure, skipping the failed spec.
    pub async fn next_fallback(
        &self,
        claude_model: &str,
        failed_spec: &str,
    ) -> Option<Arc<ModelEndpoint>> {
        let tier = detect_tier(claude_model);
        let tiers = self.tiers.read().await;

        let endpoints = tiers.get(&tier).or_else(|| tiers.get("default"))?;

        for ep in endpoints {
            if ep.full_spec == failed_spec {
                continue;
            }
            if self.is_available(ep).await {
                return Some(ep.clone());
            }
        }
        None
    }

    /// Report a successful request.
    #[allow(dead_code)]
    pub async fn report_success(&self, endpoint: &ModelEndpoint) {
        endpoint.consecutive_errors.store(0, Ordering::Relaxed);
        endpoint.healthy.store(true, Ordering::Relaxed);
    }

    /// Report an error — may trigger cooldown.
    #[allow(dead_code)]
    pub async fn report_error(&self, endpoint: &ModelEndpoint) {
        let max_errors = *self.max_consecutive_errors.read().await;
        let errors = endpoint.consecutive_errors.fetch_add(1, Ordering::Relaxed) + 1;

        if errors >= max_errors {
            let cooldown_secs = *self.cooldown_seconds.read().await;
            let until = Instant::now() + std::time::Duration::from_secs(cooldown_secs);
            let mut lock = endpoint.cooldown_until.lock().await;
            *lock = Some(until);
            endpoint.healthy.store(false, Ordering::Relaxed);
            warn!(
                "Model {} placed on {}s cooldown after {} errors",
                endpoint.full_spec, cooldown_secs, errors
            );
        }
    }

    /// Reload the router with a new profile config.
    pub async fn reload(&self, config: &ProfileConfig) {
        let new_tiers = build_tiers(&config.model_mapping);
        let new_indices: HashMap<String, AtomicUsize> = new_tiers
            .keys()
            .map(|k| (k.clone(), AtomicUsize::new(0)))
            .collect();

        {
            let mut tiers = self.tiers.write().await;
            *tiers = new_tiers;
        }
        {
            let mut indices = self.round_robin_indices.write().await;
            *indices = new_indices;
        }
        {
            let mut strategy = self.strategy.write().await;
            *strategy = RoutingStrategy::parse(&config.routing.model_strategy);
        }
        {
            let mut cooldown = self.cooldown_seconds.write().await;
            *cooldown = config.routing.rate_limit_cooldown;
        }
        {
            let mut max_err = self.max_consecutive_errors.write().await;
            *max_err = config.routing.max_consecutive_errors;
        }
        info!("Model router reloaded");
    }

    /// Get status of all model endpoints for UI display.
    pub async fn status(&self) -> HashMap<String, Vec<ModelEndpointStatus>> {
        let tiers = self.tiers.read().await;
        let mut result = HashMap::new();
        for (tier, endpoints) in tiers.iter() {
            let mut statuses = Vec::new();
            for ep in endpoints {
                let on_cooldown = {
                    let lock = ep.cooldown_until.lock().await;
                    lock.is_some_and(|until| Instant::now() < until)
                };
                statuses.push(ModelEndpointStatus {
                    provider: ep.provider.clone(),
                    model_name: ep.model_name.clone(),
                    full_spec: ep.full_spec.clone(),
                    healthy: ep.healthy.load(Ordering::Relaxed),
                    on_cooldown,
                    consecutive_errors: ep.consecutive_errors.load(Ordering::Relaxed),
                });
            }
            result.insert(tier.clone(), statuses);
        }
        result
    }

    /// Recovery: clear expired cooldowns across all tiers.
    #[allow(dead_code)]
    pub async fn recover_expired_cooldowns(&self) {
        let tiers = self.tiers.read().await;
        let now = Instant::now();
        for endpoints in tiers.values() {
            for ep in endpoints {
                let mut lock = ep.cooldown_until.lock().await;
                if lock.is_some_and(|until| now >= until) {
                    *lock = None;
                    ep.consecutive_errors.store(0, Ordering::Relaxed);
                    ep.healthy.store(true, Ordering::Relaxed);
                    info!("Model {} recovered from cooldown", ep.full_spec);
                }
            }
        }
    }

    // ── Selection strategies ─────────────────────────────────────────────────

    async fn select_round_robin(
        &self,
        tier: &str,
        endpoints: &[Arc<ModelEndpoint>],
    ) -> Option<Arc<ModelEndpoint>> {
        let indices = self.round_robin_indices.read().await;
        let idx_atom = indices.get(tier)?;
        let len = endpoints.len();
        let start = idx_atom.fetch_add(1, Ordering::Relaxed) % len;

        for i in 0..len {
            let ep = &endpoints[(start + i) % len];
            if self.is_available(ep).await {
                return Some(ep.clone());
            }
        }
        None
    }

    async fn select_random(&self, endpoints: &[Arc<ModelEndpoint>]) -> Option<Arc<ModelEndpoint>> {
        use std::time::SystemTime;
        let seed = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .subsec_nanos() as usize;

        let len = endpoints.len();
        let start = seed % len;

        for i in 0..len {
            let ep = &endpoints[(start + i) % len];
            if self.is_available(ep).await {
                return Some(ep.clone());
            }
        }
        None
    }

    async fn select_least_errors(
        &self,
        endpoints: &[Arc<ModelEndpoint>],
    ) -> Option<Arc<ModelEndpoint>> {
        let mut best: Option<&Arc<ModelEndpoint>> = None;
        let mut best_errors = u32::MAX;

        for ep in endpoints {
            if self.is_available(ep).await {
                let errors = ep.consecutive_errors.load(Ordering::Relaxed);
                if errors < best_errors {
                    best_errors = errors;
                    best = Some(ep);
                }
            }
        }
        best.cloned()
    }

    async fn is_available(&self, ep: &ModelEndpoint) -> bool {
        if !ep.healthy.load(Ordering::Relaxed) {
            // Check if cooldown has expired
            let mut lock = ep.cooldown_until.lock().await;
            if lock.is_some_and(|until| Instant::now() >= until) {
                *lock = None;
                ep.healthy.store(true, Ordering::Relaxed);
                ep.consecutive_errors.store(0, Ordering::Relaxed);
                return true;
            }
            return false;
        }
        let lock = ep.cooldown_until.lock().await;
        !matches!(*lock, Some(until) if Instant::now() < until)
    }
}

// ── Helper functions ─────────────────────────────────────────────────────────

/// Detect which Claude tier a model name belongs to.
fn detect_tier(claude_model: &str) -> String {
    let lower = claude_model.to_lowercase();
    if lower.contains("opus") {
        "opus".to_string()
    } else if lower.contains("haiku") {
        "haiku".to_string()
    } else if lower.contains("sonnet") {
        "sonnet".to_string()
    } else {
        "default".to_string()
    }
}

/// Parse a full model spec like "openrouter/moonshotai/kimi-k2.6" into
/// provider and model parts.
fn parse_model_spec(spec: &str) -> (String, String) {
    match spec.split_once('/') {
        Some((provider, model)) => (provider.to_string(), model.to_string()),
        None => ("openai".to_string(), spec.to_string()),
    }
}

/// Build tier endpoint maps from model mapping configuration.
fn build_tiers(
    mapping: &crate::panel_config::ModelMapping,
) -> HashMap<String, Vec<Arc<ModelEndpoint>>> {
    let mut tiers = HashMap::new();

    let tier_configs = [
        ("default", &mapping.default),
        ("opus", &mapping.opus),
        ("sonnet", &mapping.sonnet),
        ("haiku", &mapping.haiku),
    ];

    for (tier, raw) in tier_configs {
        let models = crate::panel_config::parse_model_list(raw);
        if models.is_empty() {
            continue;
        }

        let endpoints: Vec<Arc<ModelEndpoint>> = models
            .into_iter()
            .map(|spec| {
                let (provider, model_name) = parse_model_spec(&spec);
                Arc::new(ModelEndpoint {
                    provider,
                    model_name,
                    full_spec: spec,
                    healthy: AtomicBool::new(true),
                    consecutive_errors: AtomicU32::new(0),
                    cooldown_until: Mutex::new(None),
                })
            })
            .collect();

        tiers.insert(tier.to_string(), endpoints);
    }

    tiers
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::panel_config::{ModelMapping, ProfileConfig};

    fn test_profile() -> ProfileConfig {
        let mut p = ProfileConfig::default();
        p.model_mapping = ModelMapping {
            default: "openrouter/llama".to_string(),
            opus: "nvidia_nim/model-a ; huggingface/model-b ; openrouter/model-c".to_string(),
            sonnet: "groq/model-x".to_string(),
            haiku: String::new(),
        };
        p
    }

    #[test]
    fn test_detect_tier() {
        assert_eq!(detect_tier("claude-3-5-opus-20250101"), "opus");
        assert_eq!(detect_tier("claude-3-5-sonnet-20250101"), "sonnet");
        assert_eq!(detect_tier("claude-3-5-haiku-20250101"), "haiku");
        assert_eq!(detect_tier("some-other-model"), "default");
    }

    #[test]
    fn test_parse_model_spec() {
        let (p, m) = parse_model_spec("openrouter/moonshotai/kimi-k2.6");
        assert_eq!(p, "openrouter");
        assert_eq!(m, "moonshotai/kimi-k2.6");

        let (p, m) = parse_model_spec("gpt-4");
        assert_eq!(p, "openai");
        assert_eq!(m, "gpt-4");
    }

    #[test]
    fn test_build_tiers() {
        let profile = test_profile();
        let tiers = build_tiers(&profile.model_mapping);

        assert_eq!(tiers["opus"].len(), 3);
        assert_eq!(tiers["opus"][0].provider, "nvidia_nim");
        assert_eq!(tiers["opus"][1].provider, "huggingface");
        assert_eq!(tiers["sonnet"].len(), 1);
        assert!(!tiers.contains_key("haiku")); // Empty → not inserted
        assert_eq!(tiers["default"].len(), 1);
    }

    #[tokio::test]
    async fn test_router_resolve() {
        let profile = test_profile();
        let router = ModelRouter::from_config(&profile);

        let resolved = router.resolve("claude-3-5-opus-20250101").await.unwrap();
        assert_eq!(resolved.tier, "opus");
        assert_eq!(resolved.endpoint.provider, "nvidia_nim");
    }

    #[tokio::test]
    async fn test_router_fallback() {
        let profile = test_profile();
        let router = ModelRouter::from_config(&profile);

        let fallback = router
            .next_fallback("claude-3-5-opus-20250101", "nvidia_nim/model-a")
            .await;
        assert!(fallback.is_some());
        assert_eq!(fallback.unwrap().full_spec, "huggingface/model-b");
    }

    #[tokio::test]
    async fn test_router_haiku_falls_back_to_default() {
        let profile = test_profile();
        let router = ModelRouter::from_config(&profile);

        let resolved = router.resolve("claude-3-5-haiku-20250101").await.unwrap();
        // Haiku is empty, should fallback to default
        assert_eq!(resolved.endpoint.full_spec, "openrouter/llama");
    }
}
