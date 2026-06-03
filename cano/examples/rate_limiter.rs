//! # Rate Limiter Example — Two Workers Sharing One Budget
//!
//! Two `tokio::spawn`ed workers share a single `Arc<RateLimiter>` capped at 5 tokens/sec.
//! Each worker makes 5 calls via [`RateLimiter::acquire`]; the limiter paces all 10 calls —
//! from either worker — against the shared 5/sec budget, so the run takes ~2 seconds. The
//! timestamps make the throttling visible: calls land at ~200ms intervals
//! (1 token ÷ 5 tokens/sec).
//!
//! The bucket is configured with `with_max_tokens(1)` (no burst headroom), so pacing
//! dominates from the very first call. Raise `max_tokens` (or add `with_burst`) to admit an
//! instantaneous burst before the steady rate takes over.
//!
//! Run with:
//! ```bash
//! cargo run --example rate_limiter
//! ```

use cano::prelude::*;
use std::sync::Arc;
use std::time::{Duration, Instant};

#[tokio::main]
async fn main() {
    // 5 tokens/sec, bucket capacity 1 → one call every ~200ms, no initial burst.
    let limiter = Arc::new(RateLimiter::new(
        RateLimiterPolicy::per_second(5).with_max_tokens(1),
    ));

    println!("=== Rate limiter demo: 2 workers, shared 5 req/s budget ===\n");
    let start = Instant::now();

    let mut handles = Vec::new();
    for worker in 0..2 {
        let limiter = Arc::clone(&limiter);
        handles.push(tokio::spawn(async move {
            for call in 1..=5 {
                // Park until the shared bucket has a token, then "make the call".
                let _permit = limiter.acquire().await;
                println!(
                    "  [{:>5.2}s] worker {worker} → call {call}",
                    start.elapsed().as_secs_f64()
                );
            }
        }));
    }

    for h in handles {
        let _ = h.await;
    }

    // The bucket starts with 1 token (max_tokens = 1): that token admits the first call at
    // t≈0, and the other 9 calls are paced one per 200ms refill (5 tokens/sec), so the run
    // lasts ~9 × 200ms ≈ 1.8s. 1500ms is a conservative lower bound that won't flake.
    let elapsed = start.elapsed();
    println!(
        "\n10 calls completed in {:.2}s under a 5 req/s cap.",
        elapsed.as_secs_f64()
    );
    assert!(
        elapsed >= Duration::from_millis(1500),
        "rate limiter should have paced the run to ~1.8s, took {elapsed:?}"
    );
}
