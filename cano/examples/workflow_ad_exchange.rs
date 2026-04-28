use cano::prelude::*;
use std::time::Duration;

// ============================================================================
// State Definitions
// ============================================================================

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum AdExchangeState {
    // Entry and validation
    Start,

    // Context gathering (Split 1)
    GatherContext,

    // Bid request phase (Split 2)
    RequestBids,

    // Auction phase (Split 3)
    ScoreBids,
    RunAuction,

    // Tracking phase (Split 4)
    TrackResults,

    // Error tracking phase (Split 5)
    ErrorTracking,

    // Terminal states
    BuildResponse,
    InvalidResponse,
    Complete,
    Rejected,
    NoFill,
}

// ============================================================================
// Data Models
// ============================================================================

#[derive(Debug, Clone)]
struct AdRequest {
    request_id: String,
    placement_id: String,
    floor_price: f64,
}

#[derive(Debug, Clone)]
struct UserContext {}

#[derive(Debug, Clone)]
struct GeoContext {}

#[derive(Debug, Clone)]
struct DeviceContext {}

#[derive(Debug, Clone)]
struct BidResponse {
    partner_id: String,
    price: f64,
    creative_id: String,
    response_time_ms: u64,
}

#[derive(Debug, Clone)]
struct ScoredBid {
    bid: BidResponse,
    score: f64, // Adjusted price after quality scoring
    rank: usize,
}

#[derive(Debug, Clone)]
struct AuctionResult {
    winner: Option<ScoredBid>,
    total_bids: usize,
    auction_time_ms: u64,
}

// ============================================================================
// Phase 1: Request Validation
// ============================================================================

#[derive(Clone)]
struct ValidateRequestTask;

#[task(state = AdExchangeState)]
impl ValidateRequestTask {
    async fn run(&self, res: &Resources) -> Result<TaskResult<AdExchangeState>, CanoError> {
        let store = res.get::<MemoryStore, str>("store")?;
        let request: AdRequest = store.get("ad_request")?;

        println!("🔍 Validating request {}", request.request_id);

        // Validation logic
        if request.placement_id.is_empty() {
            println!("❌ Invalid placement ID");
            return Ok(TaskResult::Single(AdExchangeState::InvalidResponse));
        }

        if request.floor_price < 0.01 {
            println!("❌ Floor price too low");
            return Ok(TaskResult::Single(AdExchangeState::InvalidResponse));
        }

        println!("✅ Request validated");
        Ok(TaskResult::Single(AdExchangeState::GatherContext))
    }
}

// ============================================================================
// Phase 2: Context Gathering (Split 1 - All Strategy)
// ============================================================================

// Wrapper enum for heterogeneous context gathering tasks
#[derive(Clone)]
enum ContextTask {
    FetchUser,
    FetchGeo,
    DetectDevice,
}

#[task(state = AdExchangeState)]
impl ContextTask {
    async fn run(&self, res: &Resources) -> Result<TaskResult<AdExchangeState>, CanoError> {
        let store = res.get::<MemoryStore, str>("store")?;
        match self {
            ContextTask::FetchUser => {
                println!("  👤 Fetching user profile...");
                tokio::time::sleep(Duration::from_millis(50)).await;

                let user = UserContext {};

                store.put("user_context", user)?;
                println!("  ✅ User profile loaded");
                Ok(TaskResult::Single(AdExchangeState::RequestBids))
            }
            ContextTask::FetchGeo => {
                println!("  🌍 Fetching geo data...");
                tokio::time::sleep(Duration::from_millis(30)).await;

                let geo = GeoContext {};

                store.put("geo_context", geo)?;
                println!("  ✅ Geo data loaded");
                Ok(TaskResult::Single(AdExchangeState::RequestBids))
            }
            ContextTask::DetectDevice => {
                println!("  📱 Detecting device...");
                tokio::time::sleep(Duration::from_millis(20)).await;

                let device = DeviceContext {};

                store.put("device_context", device)?;
                println!("  ✅ Device detected");
                Ok(TaskResult::Single(AdExchangeState::RequestBids))
            }
        }
    }
}

// ============================================================================
// Phase 3: Bid Requests (Split 2 - PartialTimeout Strategy)
// ============================================================================

#[derive(Clone)]
struct ContactDSPTask {
    partner_id: String,
    response_delay_ms: u64, // Simulated network latency
}

#[task(state = AdExchangeState)]
impl ContactDSPTask {
    async fn run(&self, res: &Resources) -> Result<TaskResult<AdExchangeState>, CanoError> {
        let store = res.get::<MemoryStore, str>("store")?;
        println!("  📡 Requesting bid from {}...", self.partner_id);

        // Simulate DSP bid request with varying latency
        tokio::time::sleep(Duration::from_millis(self.response_delay_ms)).await;

        // Some DSPs might not respond in time or may not bid
        if self.response_delay_ms > 180 {
            // Will timeout
            tokio::time::sleep(Duration::from_millis(100)).await;
        }

        let bid = BidResponse {
            partner_id: self.partner_id.clone(),
            price: 2.50 + (self.response_delay_ms as f64 / 100.0),
            creative_id: format!("creative_{}", self.partner_id),
            response_time_ms: self.response_delay_ms,
        };

        // Store bid
        let mut bids: Vec<BidResponse> = store.get("bids").unwrap_or_default();
        bids.push(bid.clone());
        store.put("bids", bids)?;

        println!("  ✅ {} bid: ${:.2}", self.partner_id, bid.price);
        Ok(TaskResult::Single(AdExchangeState::ScoreBids))
    }
}

// ============================================================================
// Phase 4: Bid Scoring (Split 3 - Percentage Strategy)
// ============================================================================

#[derive(Clone)]
struct ScoreBidTask {
    bid_index: usize,
}

#[task(state = AdExchangeState)]
impl ScoreBidTask {
    async fn run(&self, res: &Resources) -> Result<TaskResult<AdExchangeState>, CanoError> {
        let store = res.get::<MemoryStore, str>("store")?;
        let bids: Vec<BidResponse> = store.get("bids")?;

        if self.bid_index >= bids.len() {
            return Err(CanoError::task_execution("Bid index out of range"));
        }

        let bid = &bids[self.bid_index];

        println!("  📊 Scoring bid from {}...", bid.partner_id);

        // Simulate scoring computation
        tokio::time::sleep(Duration::from_millis(10)).await;

        // Quality score based on partner history and response time
        let quality_multiplier = match bid.response_time_ms {
            0..=50 => 1.1,     // Fast response bonus
            51..=100 => 1.0,   // Normal
            101..=150 => 0.95, // Slight penalty
            _ => 0.9,          // Slow response penalty
        };

        let scored_bid = ScoredBid {
            bid: bid.clone(),
            score: bid.price * quality_multiplier,
            rank: 0, // Will be set during auction
        };

        let score_value = scored_bid.score;

        // Store scored bid
        let mut scored_bids: Vec<ScoredBid> = store.get("scored_bids").unwrap_or_default();
        scored_bids.push(scored_bid);
        store.put("scored_bids", scored_bids)?;

        println!("  ✅ Bid scored: ${:.2}", score_value);
        Ok(TaskResult::Single(AdExchangeState::RunAuction))
    }
}

// ============================================================================
// Phase 5: Auction
// ============================================================================

#[derive(Clone)]
struct RunAuctionTask;

#[task(state = AdExchangeState)]
impl RunAuctionTask {
    async fn run(&self, res: &Resources) -> Result<TaskResult<AdExchangeState>, CanoError> {
        let store = res.get::<MemoryStore, str>("store")?;
        println!("\n  🎯 Running auction...");

        let start = tokio::time::Instant::now();
        let mut scored_bids: Vec<ScoredBid> = store.get("scored_bids")?;
        let request: AdRequest = store.get("ad_request")?;

        // Filter bids above floor price
        scored_bids.retain(|b| b.score >= request.floor_price);

        if scored_bids.is_empty() {
            println!("  ❌ No valid bids above floor price");
            let result = AuctionResult {
                winner: None,
                total_bids: 0,
                auction_time_ms: start.elapsed().as_millis() as u64,
            };
            store.put("auction_result", result)?;
            return Ok(TaskResult::Single(AdExchangeState::ErrorTracking));
        }

        // Sort by score (descending)
        scored_bids.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap());

        // Set ranks
        for (i, bid) in scored_bids.iter_mut().enumerate() {
            bid.rank = i + 1;
        }

        let winner = scored_bids[0].clone();
        println!(
            "  🏆 Winner: {} at ${:.2}",
            winner.bid.partner_id, winner.score
        );

        let result = AuctionResult {
            winner: Some(winner),
            total_bids: scored_bids.len(),
            auction_time_ms: start.elapsed().as_millis() as u64,
        };

        store.put("auction_result", result)?;
        Ok(TaskResult::Single(AdExchangeState::TrackResults))
    }
}

// ============================================================================
// Phase 6: Tracking (Split 4 - Quorum Strategy)
// ============================================================================

// Wrapper enum for heterogeneous tracking tasks
#[derive(Clone)]
enum TrackingTask {
    LogAnalytics,
    UpdateMetrics,
    NotifyWinner,
    StoreAuction,
}

#[task(state = AdExchangeState)]
impl TrackingTask {
    async fn run(&self, res: &Resources) -> Result<TaskResult<AdExchangeState>, CanoError> {
        let store = res.get::<MemoryStore, str>("store")?;
        match self {
            TrackingTask::LogAnalytics => {
                println!("  📈 Logging to analytics...");
                tokio::time::sleep(Duration::from_millis(30)).await;

                let result: AuctionResult = store.get("auction_result")?;
                println!("  ✅ Analytics logged: {} bids", result.total_bids);
                Ok(TaskResult::Single(AdExchangeState::BuildResponse))
            }
            TrackingTask::UpdateMetrics => {
                println!("  📊 Updating metrics...");
                tokio::time::sleep(Duration::from_millis(25)).await;

                println!("  ✅ Metrics updated");
                Ok(TaskResult::Single(AdExchangeState::BuildResponse))
            }
            TrackingTask::NotifyWinner => {
                println!("  📬 Notifying winner...");

                let result: AuctionResult = store.get("auction_result")?;
                if let Some(winner) = result.winner {
                    tokio::time::sleep(Duration::from_millis(40)).await;
                    println!("  ✅ Winner {} notified", winner.bid.partner_id);
                }

                Ok(TaskResult::Single(AdExchangeState::BuildResponse))
            }
            TrackingTask::StoreAuction => {
                println!("  💾 Storing auction data...");
                tokio::time::sleep(Duration::from_millis(35)).await;

                println!("  ✅ Auction data stored");
                Ok(TaskResult::Single(AdExchangeState::BuildResponse))
            }
        }
    }
}

// ============================================================================
// Phase 7: Response Building
// ============================================================================

#[derive(Clone)]
struct BuildResponseTask;

#[task(state = AdExchangeState)]
impl BuildResponseTask {
    async fn run(&self, res: &Resources) -> Result<TaskResult<AdExchangeState>, CanoError> {
        let store = res.get::<MemoryStore, str>("store")?;
        println!("\n  📦 Building response...");

        let request: AdRequest = store.get("ad_request")?;
        let result: AuctionResult = store.get("auction_result")?;

        println!("\n🎯 Ad Exchange Response Summary:");
        println!("  Request ID: {}", request.request_id);
        println!("  Total Bids: {}", result.total_bids);
        println!("  Auction Time: {}ms", result.auction_time_ms);

        if let Some(winner) = result.winner {
            println!("  Winner: {}", winner.bid.partner_id);
            println!("  Winning Price: ${:.2}", winner.score);
            println!("  Creative: {}", winner.bid.creative_id);
        } else {
            println!("  Result: No Fill");
        }

        Ok(TaskResult::Single(AdExchangeState::Complete))
    }
}

// ============================================================================
// NoFill Handler
// ============================================================================

#[derive(Clone)]
struct NoFillTask;

#[task(state = AdExchangeState)]
impl NoFillTask {
    async fn run(&self, _res: &Resources) -> Result<TaskResult<AdExchangeState>, CanoError> {
        println!("\n⚠️  No Fill Response");
        println!("Unable to complete ad request due to timeout or insufficient data.\n");
        Ok(TaskResult::Single(AdExchangeState::Complete))
    }
}

// ============================================================================
// Invalid Response Handler
// ============================================================================

#[derive(Clone)]
struct InvalidResponseTask;

#[task(state = AdExchangeState)]
impl InvalidResponseTask {
    async fn run(&self, _res: &Resources) -> Result<TaskResult<AdExchangeState>, CanoError> {
        println!("\n⚠️  Invalid Request");
        println!("Request validation failed.\n");
        Ok(TaskResult::Single(AdExchangeState::Complete))
    }
}

// ============================================================================
// Phase 8: Error Tracking (Split 5 - All Strategy)
// ============================================================================

// Wrapper enum for error tracking tasks
#[derive(Clone)]
enum ErrorTrackingTask {
    LogError,
    UpdateErrorMetrics,
}

#[task(state = AdExchangeState)]
impl ErrorTrackingTask {
    async fn run(&self, res: &Resources) -> Result<TaskResult<AdExchangeState>, CanoError> {
        let store = res.get::<MemoryStore, str>("store")?;
        match self {
            ErrorTrackingTask::LogError => {
                println!("  📝 Logging error...");
                tokio::time::sleep(Duration::from_millis(20)).await;

                // Determine error type from store or state
                let error_type = if store.get::<AuctionResult>("auction_result").is_ok() {
                    "NoFill"
                } else {
                    "Rejected"
                };

                println!("  ✅ Error logged: {}", error_type);
                Ok(TaskResult::Single(AdExchangeState::NoFill))
            }
            ErrorTrackingTask::UpdateErrorMetrics => {
                println!("  📊 Updating error metrics...");
                tokio::time::sleep(Duration::from_millis(25)).await;

                println!("  ✅ Error metrics updated");
                Ok(TaskResult::Single(AdExchangeState::NoFill))
            }
        }
    }
}

// ============================================================================
// Main Workflow Construction
// ============================================================================

fn create_ad_exchange_workflow(store: MemoryStore) -> Workflow<AdExchangeState> {
    Workflow::new(Resources::new().insert("store", store.clone()))
        // Phase 1: Validation
        .register(AdExchangeState::Start, ValidateRequestTask)
        // Invalid Response Handler
        .register(AdExchangeState::InvalidResponse, InvalidResponseTask)
        // Phase 2: Context Gathering - SPLIT 1 (All Strategy)
        // All three must succeed to proceed within 100ms timeout
        // If any task fails or timeout is exceeded, workflow will error and transition to NoFill
        .register_split(
            AdExchangeState::GatherContext,
            vec![
                ContextTask::FetchUser,
                ContextTask::FetchGeo,
                ContextTask::DetectDevice,
            ],
            JoinConfig::new(JoinStrategy::All, AdExchangeState::RequestBids)
                .with_timeout(Duration::from_millis(100)),
        )
        // Phase 3: Bid Requests - SPLIT 2 (PartialTimeout Strategy)
        // Accept whatever bids come back within 200ms
        .register_split(
            AdExchangeState::RequestBids,
            vec![
                ContactDSPTask {
                    partner_id: "DSP-FastBidder".to_string(),
                    response_delay_ms: 45,
                },
                ContactDSPTask {
                    partner_id: "DSP-Premium".to_string(),
                    response_delay_ms: 80,
                },
                ContactDSPTask {
                    partner_id: "DSP-Global".to_string(),
                    response_delay_ms: 120,
                },
                ContactDSPTask {
                    partner_id: "DSP-Slow".to_string(),
                    response_delay_ms: 190,
                },
                ContactDSPTask {
                    partner_id: "DSP-TooSlow".to_string(),
                    response_delay_ms: 250,
                },
            ],
            JoinConfig::new(JoinStrategy::PartialTimeout, AdExchangeState::ScoreBids)
                .with_timeout(Duration::from_millis(200)),
        )
        // Phase 4: Bid Scoring - SPLIT 3 (All Strategy)
        // Score all received bids within 50ms timeout
        // If timeout or any scoring fails, workflow will error and transition to NoFill
        .register_split(
            AdExchangeState::ScoreBids,
            vec![
                ScoreBidTask { bid_index: 0 },
                ScoreBidTask { bid_index: 1 },
                ScoreBidTask { bid_index: 2 },
            ],
            JoinConfig::new(JoinStrategy::All, AdExchangeState::RunAuction)
                .with_timeout(Duration::from_millis(50)),
        )
        // Phase 5: Auction
        .register(AdExchangeState::RunAuction, RunAuctionTask)
        // Phase 6: Tracking - SPLIT 4 (All Strategy)
        // All tracking tasks must complete within 100ms timeout
        // If timeout or any task fails, workflow will error and transition to NoFill
        .register_split(
            AdExchangeState::TrackResults,
            vec![
                TrackingTask::LogAnalytics,
                TrackingTask::UpdateMetrics,
                TrackingTask::NotifyWinner,
                TrackingTask::StoreAuction,
            ],
            JoinConfig::new(JoinStrategy::All, AdExchangeState::BuildResponse)
                .with_timeout(Duration::from_millis(100)),
        )
        // Phase 7: Response
        .register(AdExchangeState::BuildResponse, BuildResponseTask)
        // NoFill handler (used when splits timeout or fail)
        .register(AdExchangeState::NoFill, NoFillTask)
        // Phase 8: Error Tracking - SPLIT 5 (All Strategy)
        // Both error logging and metrics must complete within 50ms timeout
        .register_split(
            AdExchangeState::ErrorTracking,
            vec![
                ErrorTrackingTask::LogError,
                ErrorTrackingTask::UpdateErrorMetrics,
            ],
            JoinConfig::new(JoinStrategy::All, AdExchangeState::NoFill)
                .with_timeout(Duration::from_millis(50)),
        )
        // Terminal states
        .add_exit_states(vec![AdExchangeState::Complete, AdExchangeState::Rejected])
}

// ============================================================================
// Example Usage
// ============================================================================

#[tokio::main]
async fn main() -> Result<(), CanoError> {
    println!("🚀 Real-Time Ad Exchange Workflow\n");
    println!("{}", "=".repeat(60));

    let store = MemoryStore::new();

    // Create ad request
    let request = AdRequest {
        request_id: "req_abc123".to_string(),
        placement_id: "placement_728x90_top".to_string(),
        floor_price: 1.50,
    };

    store.put("ad_request", request)?;

    // Build and execute workflow
    let workflow = create_ad_exchange_workflow(store.clone());

    println!("\n🎬 Starting ad exchange workflow...\n");
    let start = tokio::time::Instant::now();

    // Execute workflow - if splits timeout or fail, transition to NoFill
    let result = match workflow.orchestrate(AdExchangeState::Start).await {
        Ok(state) => state,
        Err(e) => {
            // If workflow fails due to split timeout/error, handle as NoFill.
            // Reuse the original workflow/store so error tracking sees the real
            // request and any context produced before the failure.
            eprintln!("❌ Workflow error: {}", e);
            store.put("error_reason", e.to_string())?;
            println!("\n⚠️  Handling as No Fill due to error\n");

            workflow.orchestrate(AdExchangeState::ErrorTracking).await?
        }
    };

    let total_time = start.elapsed();

    println!("\n{}", "=".repeat(60));
    println!("✅ Workflow completed in {:?}", total_time);
    println!("   Final State: {:?}", result);

    Ok(())
}
