//! # Multi-Level Rate Limiter — Claude-Code-style tiers
//!
//! Enforces several limits at once on one logical action — a short "5-hour" window, a "weekly"
//! window, a per-model weekly window, and a usage/token budget. A request is admitted only if
//! **every applicable tier** has room; a rejection reports **which** tier blocked it and the
//! retry-after via [`CanoError::RateLimited`]. Mixes [`WindowedRateLimiter`] (fixed window, with a
//! real reset boundary) and [`RateLimiter`] (continuous token bucket) freely, each metering its
//! own currency via the tier `cost`.
//!
//! Durations are scaled to hundreds of ms so the demo finishes in ~half a second; in production
//! these would be hours/days.
//!
//! Run with:
//! ```bash
//! cargo run --example rate_limiter_multi
//! ```

use cano::prelude::*;
use std::sync::Arc;
use std::time::Duration;

#[tokio::main]
async fn main() {
    let five_hour: Arc<dyn Meter> = Arc::new(WindowedRateLimiter::new(WindowPolicy::new(
        3,
        Duration::from_millis(400),
    )));
    let weekly: Arc<dyn Meter> = Arc::new(WindowedRateLimiter::new(WindowPolicy::new(
        20,
        Duration::from_secs(10),
    )));
    let opus_weekly: Arc<dyn Meter> = Arc::new(WindowedRateLimiter::new(WindowPolicy::new(
        2,
        Duration::from_secs(10),
    )));
    // A usage/token budget: a smooth bucket of 10k tokens refilling at 10k/sec; each call costs 1500.
    let tokens: Arc<dyn Meter> = Arc::new(RateLimiter::new(
        RateLimiterPolicy::new(10_000, Duration::from_secs(1)).with_max_tokens(10_000),
    ));

    let limiter = MultiRateLimiter::new()
        .with_tier("5h", Arc::clone(&five_hour), 1)
        .with_tier("weekly", Arc::clone(&weekly), 1)
        .with_tier("opus_weekly", Arc::clone(&opus_weekly), 1)
        .with_tier("tokens", Arc::clone(&tokens), 1500);

    println!("=== Multi-level rate limiter ===");
    println!("tiers: 5h(3/400ms)  weekly(20/10s)  opus_weekly(2/10s)  tokens(10k/s, 1500/call)\n");

    // ---- Phase 1: shed-load. The per-model weekly cap (2) blocks the 3rd Opus request. ----
    println!("Phase 1 — Opus requests (try_acquire; opus_weekly cap = 2):");
    for i in 1..=3 {
        report(i, limiter.try_acquire(), &five_hour, &opus_weekly);
    }
    println!(
        "  -> 5h tier used = {} (only the 2 ADMITTED calls; the rejected one refunded its \
         5h/weekly debit — opus_weekly rejected before the tokens tier was reached — no leak)\n",
        used(&five_hour)
    );

    // ---- Phase 2: per-request tier selection. A non-Opus request skips opus_weekly. ----
    println!("Phase 2 — a non-Opus request (try_acquire_for skips opus_weekly):");
    match limiter.try_acquire_for(&["5h", "weekly", "tokens"]) {
        Ok(_) => println!(
            "  admitted; opus_weekly left untouched (used = {})\n",
            used(&opus_weekly)
        ),
        Err(e) => println!("  blocked: {e}\n"),
    }

    // ---- Phase 3: async park. The 5h window is now full; acquire() waits for its reset. ----
    println!("Phase 3 — 5h window is full; acquire().await parks until it resets:");
    let start = std::time::Instant::now();
    let _permit = limiter.acquire_for(&["5h", "weekly", "tokens"]).await;
    let waited = start.elapsed();
    println!("  admitted after parking {waited:?} (waited for the 5h window to reset)");
    assert!(
        waited >= Duration::from_millis(100),
        "should have parked for the window reset, waited {waited:?}"
    );

    println!("\nDone — every admission satisfied all applicable tiers simultaneously.");
}

fn used(m: &Arc<dyn Meter>) -> u64 {
    m.snapshot().used
}

fn report(
    i: usize,
    r: Result<MultiPermit, CanoError>,
    five_hour: &Arc<dyn Meter>,
    opus: &Arc<dyn Meter>,
) {
    match r {
        Ok(_permit) => println!(
            "  req {i}: admitted (5h used = {}, opus_weekly used = {})",
            used(five_hour),
            used(opus)
        ),
        Err(CanoError::RateLimited { tier, retry_after }) => {
            println!("  req {i}: BLOCKED by `{tier}` (retry after {retry_after:?})")
        }
        Err(e) => println!("  req {i}: error: {e}"),
    }
}
