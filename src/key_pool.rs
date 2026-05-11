//! API key pooling engine with health tracking and load balancing.
//!
//! Supports multiple keys per provider with:
//! - Round-robin, random, or least-errors selection strategy
//! - Automatic cooldown for rate-limited keys
//! - Health recovery after cooldown period
//! - Per-key statistics (requests, errors, latency)

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::time::Instant;

use tokio::sync::{Mutex, RwLock};
use tracing::{info, warn};

use crate::panel_config::{ProfileConfig, RoutingConfig, RoutingStrategy};

/// A single API key endpoint with health tracking.
pub struct KeyEndpoint {
    pub key: String,
    pub provider: String,
    pub healthy: AtomicBool,
    pub consecutive_errors: AtomicU32,
    /// Tracks how many times this key has entered cooldown (for exponential backoff).
    pub cooldown_count: AtomicU32,
    pub cooldown_until: Mutex<Option<Instant>>,
    pub total_requests: AtomicU64,
    pub total_errors: AtomicU64,
    pub last_latency_ms: AtomicU64,
}

/// Serializable status snapshot of a key endpoint for UI display.
#[derive(Debug, Clone, serde::Serialize)]
pub struct KeyEndpointStatus {
    /// Masked key (first 8 chars + "...")
    pub key_preview: String,
    pub provider: String,
    pub healthy: bool,
    pub on_cooldown: bool,
    pub total_requests: u64,
    pub total_errors: u64,
    pub last_latency_ms: u64,
}

/// Pool of keys for a single provider.
pub struct KeyPool {
    endpoints: Vec<Arc<KeyEndpoint>>,
    strategy: RoutingStrategy,
    round_robin_idx: AtomicUsize,
    cooldown_seconds: u64,
    max_consecutive_errors: u32,
}

impl KeyPool {
    /// Create a new key pool from a list of keys for a provider.
    pub fn new(keys: Vec<String>, provider: String, config: &RoutingConfig) -> Self {
        let endpoints = keys
            .into_iter()
            .map(|key| {
                Arc::new(KeyEndpoint {
                    key,
                    provider: provider.clone(),
                    healthy: AtomicBool::new(true),
                    consecutive_errors: AtomicU32::new(0),
                    cooldown_count: AtomicU32::new(0),
                    cooldown_until: Mutex::new(None),
                    total_requests: AtomicU64::new(0),
                    total_errors: AtomicU64::new(0),
                    last_latency_ms: AtomicU64::new(0),
                })
            })
            .collect();

        Self {
            endpoints,
            strategy: RoutingStrategy::parse(&config.key_strategy),
            round_robin_idx: AtomicUsize::new(0),
            cooldown_seconds: config.rate_limit_cooldown,
            max_consecutive_errors: config.max_consecutive_errors,
        }
    }

    /// Select the next healthy key using the configured strategy.
    /// Returns `None` if all keys are on cooldown.
    ///
    /// **Concurrency note**: this method does **not** reserve the chosen key
    /// for the duration of the in-flight request. With many concurrent
    /// requests targeting a pool that has only a single healthy key, multiple
    /// callers will pick the same endpoint and may all hit a 429 in lock-step
    /// before any of them reports an error. This is mitigated by:
    ///
    /// 1. The provider-level `RateLimiter::acquire_concurrency()` semaphore
    ///    that throttles the global parallel request count.
    /// 2. Round-robin distribution across multiple keys when more than one is
    ///    healthy.
    ///
    /// For pools with a single key under bursty load, prefer increasing the
    /// number of keys or lowering `max_concurrency` in routing config.
    pub async fn acquire(&self) -> Option<Arc<KeyEndpoint>> {
        if self.endpoints.is_empty() {
            return None;
        }

        // Single key fast path
        if self.endpoints.len() == 1 {
            let ep = &self.endpoints[0];
            if self.is_available(ep).await {
                ep.total_requests.fetch_add(1, Ordering::Relaxed);
                return Some(ep.clone());
            }
            return None;
        }

        match self.strategy {
            RoutingStrategy::RoundRobin => self.acquire_round_robin().await,
            RoutingStrategy::Random => self.acquire_random().await,
            RoutingStrategy::LeastErrors => self.acquire_least_errors().await,
        }
    }

    async fn acquire_round_robin(&self) -> Option<Arc<KeyEndpoint>> {
        let len = self.endpoints.len();
        let start = self.round_robin_idx.fetch_add(1, Ordering::Relaxed) % len;

        for i in 0..len {
            let idx = (start + i) % len;
            let ep = &self.endpoints[idx];
            if self.is_available(ep).await {
                ep.total_requests.fetch_add(1, Ordering::Relaxed);
                return Some(ep.clone());
            }
        }
        None
    }

    async fn acquire_random(&self) -> Option<Arc<KeyEndpoint>> {
        use std::time::SystemTime;
        let seed = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .subsec_nanos() as usize;

        let len = self.endpoints.len();
        let start = seed % len;

        for i in 0..len {
            let idx = (start + i) % len;
            let ep = &self.endpoints[idx];
            if self.is_available(ep).await {
                ep.total_requests.fetch_add(1, Ordering::Relaxed);
                return Some(ep.clone());
            }
        }
        None
    }

    async fn acquire_least_errors(&self) -> Option<Arc<KeyEndpoint>> {
        let mut best: Option<&Arc<KeyEndpoint>> = None;
        let mut best_errors = u64::MAX;

        for ep in &self.endpoints {
            if self.is_available(ep).await {
                let errors = ep.total_errors.load(Ordering::Relaxed);
                if errors < best_errors {
                    best_errors = errors;
                    best = Some(ep);
                }
            }
        }

        best.map(|ep| {
            ep.total_requests.fetch_add(1, Ordering::Relaxed);
            ep.clone()
        })
    }

    async fn is_available(&self, ep: &KeyEndpoint) -> bool {
        if ep.healthy.load(Ordering::Relaxed) {
            return true;
        }
        // Check if cooldown has expired
        let mut lock = ep.cooldown_until.lock().await;
        if lock.is_some_and(|until| Instant::now() >= until) {
            *lock = None;
            ep.healthy.store(true, Ordering::Relaxed);
            ep.consecutive_errors.store(0, Ordering::Relaxed);
            return true;
        }
        false
    }

    /// Report a successful request — resets error counter and cooldown escalation.
    pub async fn report_success(&self, endpoint: &KeyEndpoint, latency_ms: u64) {
        endpoint
            .last_latency_ms
            .store(latency_ms, Ordering::Relaxed);

        let cooldown_active = {
            let lock = endpoint.cooldown_until.lock().await;
            lock.is_some_and(|until| Instant::now() < until)
        };
        if cooldown_active {
            return;
        }

        endpoint.consecutive_errors.store(0, Ordering::Relaxed);
        endpoint.cooldown_count.store(0, Ordering::Relaxed);
        endpoint.healthy.store(true, Ordering::Relaxed);
    }

    /// Report an error — increments counter, may trigger cooldown.
    /// Uses **exponential cooldown** for rate limits: `base * 2^(n-1)`, capped at 300s.
    pub async fn report_error(&self, endpoint: &KeyEndpoint, is_rate_limit: bool) {
        endpoint.total_errors.fetch_add(1, Ordering::Relaxed);
        let errors = endpoint.consecutive_errors.fetch_add(1, Ordering::Relaxed) + 1;

        if is_rate_limit || errors >= self.max_consecutive_errors {
            // Increment cooldown count for exponential backoff
            let cd_count = endpoint.cooldown_count.fetch_add(1, Ordering::Relaxed) + 1;

            let cooldown = if is_rate_limit {
                // Exponential: base * 2^(n-1), capped at 300s (5 min)
                let exp = self
                    .cooldown_seconds
                    .saturating_mul(2u64.saturating_pow(cd_count.saturating_sub(1)));
                exp.min(300)
            } else {
                self.cooldown_seconds / 2
            };

            let until = Instant::now() + std::time::Duration::from_secs(cooldown);
            let mut lock = endpoint.cooldown_until.lock().await;
            *lock = Some(until);
            endpoint.healthy.store(false, Ordering::Relaxed);

            let preview = mask_key(&endpoint.key);
            warn!(
                "Key {} ({}) placed on {}s cooldown ({}, escalation={})",
                preview,
                endpoint.provider,
                cooldown,
                if is_rate_limit {
                    "rate limited"
                } else {
                    "consecutive errors"
                },
                cd_count,
            );
        }
    }

    /// Returns true if ALL keys in this pool are currently on cooldown.
    pub async fn all_exhausted(&self) -> bool {
        if self.endpoints.is_empty() {
            return true;
        }
        for ep in &self.endpoints {
            if self.is_available(ep).await {
                return false;
            }
        }
        true
    }

    /// Get status snapshots of all endpoints for UI display.
    pub async fn status(&self) -> Vec<KeyEndpointStatus> {
        let mut statuses = Vec::with_capacity(self.endpoints.len());
        for ep in &self.endpoints {
            let on_cooldown = {
                let lock = ep.cooldown_until.lock().await;
                lock.is_some_and(|until| Instant::now() < until)
            };
            statuses.push(KeyEndpointStatus {
                key_preview: mask_key(&ep.key),
                provider: ep.provider.clone(),
                healthy: ep.healthy.load(Ordering::Relaxed),
                on_cooldown,
                total_requests: ep.total_requests.load(Ordering::Relaxed),
                total_errors: ep.total_errors.load(Ordering::Relaxed),
                last_latency_ms: ep.last_latency_ms.load(Ordering::Relaxed),
            });
        }
        statuses
    }

    /// Recovery task: clear expired cooldowns periodically.
    pub async fn recover_expired_cooldowns(&self) {
        let now = Instant::now();
        for ep in &self.endpoints {
            let mut lock = ep.cooldown_until.lock().await;
            if lock.is_some_and(|until| now >= until) {
                *lock = None;
                ep.consecutive_errors.store(0, Ordering::Relaxed);
                ep.healthy.store(true, Ordering::Relaxed);
                let preview = mask_key(&ep.key);
                info!("Key {} ({}) recovered from cooldown", preview, ep.provider);
            }
        }
    }
}

/// Manager that holds key pools for all providers.
pub struct KeyPoolManager {
    pools: RwLock<HashMap<String, Arc<KeyPool>>>,
    routing_config: RwLock<RoutingConfig>,
}

impl KeyPoolManager {
    /// Build the manager from a profile configuration.
    pub fn from_config(config: &ProfileConfig) -> Self {
        let mut pools = HashMap::new();
        for provider in config.provider_keys.keys() {
            let keys = config.keys_for_provider(provider);
            if !keys.is_empty() {
                pools.insert(
                    provider.clone(),
                    Arc::new(KeyPool::new(keys, provider.clone(), &config.routing)),
                );
            }
        }

        Self {
            pools: RwLock::new(pools),
            routing_config: RwLock::new(config.routing.clone()),
        }
    }

    /// Get a key for the given provider.
    pub async fn acquire(&self, provider: &str) -> Option<Arc<KeyEndpoint>> {
        let pools = self.pools.read().await;
        if let Some(pool) = pools.get(provider) {
            pool.acquire().await
        } else {
            None
        }
    }

    /// Report success for a key endpoint.
    pub async fn report_success(&self, endpoint: &KeyEndpoint, latency_ms: u64) {
        let pools = self.pools.read().await;
        if let Some(pool) = pools.get(&endpoint.provider) {
            pool.report_success(endpoint, latency_ms).await;
        }
    }

    /// Report error for a key endpoint.
    pub async fn report_error(&self, endpoint: &KeyEndpoint, is_rate_limit: bool) {
        let pools = self.pools.read().await;
        if let Some(pool) = pools.get(&endpoint.provider) {
            pool.report_error(endpoint, is_rate_limit).await;
        }
    }

    /// Check if all keys for a given provider are exhausted (on cooldown).
    pub async fn all_exhausted(&self, provider: &str) -> bool {
        let pools = self.pools.read().await;
        match pools.get(provider) {
            Some(pool) => pool.all_exhausted().await,
            None => true, // No pool = considered exhausted
        }
    }

    pub async fn has_pool(&self, provider: &str) -> bool {
        self.pools.read().await.contains_key(provider)
    }

    /// Reload pools from a new profile config.
    pub async fn reload(&self, config: &ProfileConfig) {
        let mut pools = self.pools.write().await;
        pools.clear();
        let routing = config.routing.clone();
        for provider in config.provider_keys.keys() {
            let keys = config.keys_for_provider(provider);
            if !keys.is_empty() {
                pools.insert(
                    provider.clone(),
                    Arc::new(KeyPool::new(keys, provider.clone(), &routing)),
                );
            }
        }
        let mut routing_config = self.routing_config.write().await;
        *routing_config = routing;
        info!("Key pools reloaded");
    }

    /// Get status of all pools for UI display.
    pub async fn status(&self) -> HashMap<String, Vec<KeyEndpointStatus>> {
        let pools = self.pools.read().await;
        let mut result = HashMap::new();
        for (provider, pool) in pools.iter() {
            result.insert(provider.clone(), pool.status().await);
        }
        result
    }

    /// Run recovery checks across all pools.
    pub async fn recover_all(&self) {
        let pools = self.pools.read().await;
        for pool in pools.values() {
            pool.recover_expired_cooldowns().await;
        }
    }

    /// Spawn a background recovery task that periodically clears expired cooldowns.
    ///
    /// The first sleep uses `initial_interval_secs`, but every subsequent
    /// iteration re-reads `routing_config.health_recovery_interval`, so config
    /// reloads (UI / hot-swap) take effect at the next tick without needing a
    /// task restart.
    pub fn spawn_recovery_task(
        self: &Arc<Self>,
        initial_interval_secs: u64,
    ) -> tokio::task::JoinHandle<()> {
        let manager = self.clone();
        tokio::spawn(async move {
            let mut current_interval = initial_interval_secs.max(1);
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(current_interval)).await;
                manager.recover_all().await;
                let cfg = manager.routing_config.read().await;
                current_interval = cfg.health_recovery_interval.max(1);
            }
        })
    }
}

/// Mask a key for display: show first 3 + "..." + last 3 chars.
pub fn mask_key(key: &str) -> String {
    let chars: Vec<char> = key.chars().collect();
    if chars.len() <= 8 {
        "***".to_string()
    } else {
        let prefix: String = chars.iter().take(3).copied().collect();
        let suffix: String = chars
            .iter()
            .rev()
            .take(3)
            .copied()
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        format!("{}...{}", prefix, suffix)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_routing_config() -> RoutingConfig {
        RoutingConfig {
            key_strategy: "round_robin".to_string(),
            model_strategy: "round_robin".to_string(),
            rate_limit_cooldown: 5,
            max_consecutive_errors: 2,
            health_recovery_interval: 10,
        }
    }

    #[tokio::test]
    async fn test_single_key_pool() {
        let config = test_routing_config();
        let pool = KeyPool::new(vec!["key1".to_string()], "test".to_string(), &config);
        let ep = pool.acquire().await;
        assert!(ep.is_some());
        assert_eq!(ep.unwrap().key, "key1");
    }

    #[tokio::test]
    async fn test_round_robin() {
        let config = test_routing_config();
        let pool = KeyPool::new(
            vec!["a".to_string(), "b".to_string(), "c".to_string()],
            "test".to_string(),
            &config,
        );

        let e1 = pool.acquire().await.unwrap();
        let e2 = pool.acquire().await.unwrap();
        let e3 = pool.acquire().await.unwrap();
        let e4 = pool.acquire().await.unwrap();

        assert_eq!(e1.key, "a");
        assert_eq!(e2.key, "b");
        assert_eq!(e3.key, "c");
        assert_eq!(e4.key, "a"); // Wraps around
    }

    #[tokio::test]
    async fn test_cooldown_skips_key() {
        let config = test_routing_config();
        let pool = KeyPool::new(
            vec!["good".to_string(), "bad".to_string()],
            "test".to_string(),
            &config,
        );

        // Mark "bad" (index 1) as rate-limited
        let _good = pool.acquire().await.unwrap(); // gets "good" (idx 0)
        let bad = pool.acquire().await.unwrap(); // gets "bad" (idx 1)
        pool.report_error(&bad, true).await;

        // Next acquisitions should skip "bad"
        let next = pool.acquire().await.unwrap();
        assert_eq!(next.key, "good");
    }

    #[tokio::test]
    async fn test_all_keys_cooled_returns_none() {
        let config = test_routing_config();
        let pool = KeyPool::new(vec!["only".to_string()], "test".to_string(), &config);

        let ep = pool.acquire().await.unwrap();
        pool.report_error(&ep, true).await;

        let result = pool.acquire().await;
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_mask_key() {
        assert_eq!(mask_key("sk-or-v1-abcdef123456"), "sk-...456");
        assert_eq!(mask_key("short"), "***");
    }

    #[tokio::test]
    async fn test_key_pool_manager_from_config() {
        let mut profile = ProfileConfig::default();
        profile
            .provider_keys
            .insert("openrouter".to_string(), "k1 ; k2".to_string());

        let manager = KeyPoolManager::from_config(&profile);
        let ep = manager.acquire("openrouter").await;
        assert!(ep.is_some());
        assert!(manager.acquire("unknown").await.is_none());
    }
}
