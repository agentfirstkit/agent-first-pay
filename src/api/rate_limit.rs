//! Token-bucket rate limiting for the HTTP API, configured by `rate_limit` in
//! the runtime config. Absent config means no limiter at all rather than a
//! generous default: the operator decides whether this daemon is behind
//! something that already paces callers.

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use crate::types::RateLimitConfig;

pub(crate) struct RateLimiter {
    /// Requests per second, which is also the refill rate.
    rps: u32,
    /// Maximum in-flight requests; 0 disables the check.
    max_concurrent: u32,
    in_flight: AtomicU32,
    /// Available tokens, scaled by 1000 so a sub-request-per-second refill
    /// still accumulates.
    tokens_milli: AtomicU64,
    last_refill_ms: AtomicU64,
}

impl RateLimiter {
    pub(crate) fn new(config: &RateLimitConfig) -> Self {
        let rps = config.requests_per_second;
        Self {
            rps,
            max_concurrent: config.max_concurrent,
            in_flight: AtomicU32::new(0),
            tokens_milli: AtomicU64::new(u64::from(rps) * 1000),
            last_refill_ms: AtomicU64::new(now_ms()),
        }
    }

    /// Take a permit for one request. The returned guard releases the
    /// concurrency slot when the response is done.
    pub(crate) fn try_acquire(&self) -> Result<RateLimitGuard<'_>, ()> {
        if self.max_concurrent > 0 {
            let previous = self.in_flight.fetch_add(1, Ordering::Relaxed);
            if previous >= self.max_concurrent {
                self.in_flight.fetch_sub(1, Ordering::Relaxed);
                return Err(());
            }
        }

        if self.rps > 0 {
            self.refill();
            let cost = 1000u64;
            loop {
                let current = self.tokens_milli.load(Ordering::Relaxed);
                if current < cost {
                    if self.max_concurrent > 0 {
                        self.in_flight.fetch_sub(1, Ordering::Relaxed);
                    }
                    return Err(());
                }
                if self
                    .tokens_milli
                    .compare_exchange_weak(
                        current,
                        current - cost,
                        Ordering::Relaxed,
                        Ordering::Relaxed,
                    )
                    .is_ok()
                {
                    break;
                }
            }
        }

        Ok(RateLimitGuard { limiter: self })
    }

    fn refill(&self) {
        let now = now_ms();
        let last = self.last_refill_ms.load(Ordering::Relaxed);
        let elapsed = now.saturating_sub(last);
        if elapsed == 0 {
            return;
        }
        if self
            .last_refill_ms
            .compare_exchange(last, now, Ordering::Relaxed, Ordering::Relaxed)
            .is_ok()
        {
            // Milli-tokens per millisecond is exactly the requests-per-second
            // rate, so no scaling is needed here.
            let add = elapsed * u64::from(self.rps);
            let max = u64::from(self.rps) * 1000;
            let _ =
                self.tokens_milli
                    .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
                        Some(current.saturating_add(add).min(max))
                    });
        }
    }
}

pub(crate) struct RateLimitGuard<'a> {
    limiter: &'a RateLimiter,
}

impl Drop for RateLimitGuard<'_> {
    fn drop(&mut self) {
        if self.limiter.max_concurrent > 0 {
            self.limiter.in_flight.fetch_sub(1, Ordering::Relaxed);
        }
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}
