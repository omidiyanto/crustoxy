use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::{Mutex, Semaphore};
use tracing::warn;

pub struct RateLimiter {
    rate_limit: u32,
    rate_window: f64,
    request_times: Mutex<VecDeque<Instant>>,
    blocked_until: Mutex<Option<Instant>>,
    concurrency_sem: Arc<Semaphore>,
}

impl RateLimiter {
    pub fn new(rate_limit: u32, rate_window: u64, max_concurrency: usize) -> Arc<Self> {
        Arc::new(Self {
            rate_limit,
            rate_window: rate_window as f64,
            request_times: Mutex::new(VecDeque::new()),
            blocked_until: Mutex::new(None),
            concurrency_sem: Arc::new(Semaphore::new(max_concurrency)),
        })
    }

    pub async fn acquire(&self) {
        {
            let blocked = self.blocked_until.lock().await;
            if let Some(until) = *blocked {
                let now = Instant::now();
                if now < until {
                    let wait = until - now;
                    warn!("Rate limit active, waiting {:.1}s", wait.as_secs_f64());
                    drop(blocked);
                    tokio::time::sleep(wait).await;
                }
            }
        }

        loop {
            let wait_time;
            {
                let mut times = self.request_times.lock().await;
                let now = Instant::now();
                let cutoff = now - std::time::Duration::from_secs_f64(self.rate_window);

                while times.front().is_some_and(|&t| t <= cutoff) {
                    times.pop_front();
                }

                if (times.len() as u32) < self.rate_limit {
                    times.push_back(now);
                    return;
                }

                let oldest = times[0];
                wait_time = (oldest + std::time::Duration::from_secs_f64(self.rate_window)) - now;
            }
            tokio::time::sleep(wait_time).await;
        }
    }

    pub async fn acquire_concurrency(&self) -> tokio::sync::OwnedSemaphorePermit {
        self.concurrency_sem.clone().acquire_owned().await.unwrap()
    }

    pub async fn set_blocked(&self, seconds: f64) {
        let mut blocked = self.blocked_until.lock().await;
        *blocked = Some(Instant::now() + std::time::Duration::from_secs_f64(seconds));
        warn!("Rate limit block set for {:.1}s", seconds);
    }

    pub async fn clear_block(&self) {
        let mut blocked = self.blocked_until.lock().await;
        *blocked = None;
        tracing::info!("Rate limit block has been cleared");
    }

    #[allow(dead_code)]
    pub async fn is_blocked(&self) -> bool {
        let blocked = self.blocked_until.lock().await;
        blocked.is_some_and(|until| Instant::now() < until)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn test_rate_limiter_not_blocked_initially() {
        let limiter = RateLimiter::new(10, 60, 5);
        assert!(!limiter.is_blocked().await);
    }

    #[tokio::test]
    async fn test_rate_limiter_set_and_clear_block() {
        let limiter = RateLimiter::new(10, 60, 5);
        limiter.set_blocked(10.0).await;
        assert!(limiter.is_blocked().await);

        limiter.clear_block().await;
        assert!(!limiter.is_blocked().await);
    }

    #[tokio::test]
    async fn test_rate_limiter_sliding_window() {
        let limiter = RateLimiter::new(2, 1, 5);
        let start = Instant::now();

        // First 2 requests should be immediate
        limiter.acquire().await;
        limiter.acquire().await;
        let diff = start.elapsed();
        assert!(diff < Duration::from_millis(100)); // Essentially instant

        // 3rd request should wait at least 1 second
        limiter.acquire().await;
        let diff2 = start.elapsed();
        assert!(diff2 >= Duration::from_millis(900)); // We allow a bit of timing jitter
    }
}
