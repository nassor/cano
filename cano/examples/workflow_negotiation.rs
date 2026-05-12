//! # Negotiation Workflow Example
//!
//! This example demonstrates a negotiation workflow between a seller and buyer using the Workflow structure:
//! 1. **SellerTask**: Starts with an initial price and decrements it on each round
//! 2. **BuyerTask**: Evaluates the offer against their budget and decides to accept or continue
//!
//! The workflow showcases:
//! - Inter-task communication through shared store
//! - Iterative negotiation logic with random price decrements
//! - Workflow control based on negotiation outcomes
//!
//! ## Testing Different Scenarios
//!
//! To test a "no deal" scenario, modify the buyer budget in the code:
//! - Change `buyer_budget = 1000` to `buyer_budget = 50` for a failed negotiation
//! - The buyer will walk away after 10 rounds if the offer is still too high
//!
//! Run with:
//! ```bash
//! cargo run --example workflow_negotiation
//! ```

use cano::prelude::*;
use rand::RngExt;

/// Action enum for controlling negotiation workflow
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum NegotiationAction {
    StartSelling,
    BuyerEvaluate,
    Deal,
    NoDeal,
    Error,
}

/// Negotiation state to track the current offer and round
#[derive(Debug, Clone)]
struct NegotiationState {
    current_offer: u32,
    buyer_budget: u32,
    round: u32,
    seller_initial_price: u32,
}

impl NegotiationState {
    fn new(initial_price: u32, buyer_budget: u32) -> Self {
        Self {
            current_offer: initial_price,
            buyer_budget,
            round: 1,
            seller_initial_price: initial_price,
        }
    }
}

/// Seller task: manages pricing and decrements offers
#[derive(Clone)]
struct SellerTask;

impl SellerTask {
    fn new() -> Self {
        Self
    }

    /// Generate a random price reduction between 500 and 2000
    fn calculate_price_reduction() -> u32 {
        let mut rng = rand::rng();
        rng.random_range(500..=2000)
    }
}

#[task(state = NegotiationAction)]
impl SellerTask {
    fn config(&self) -> TaskConfig {
        TaskConfig::minimal()
    }

    async fn run(&self, res: &Resources) -> Result<TaskResult<NegotiationAction>, CanoError> {
        let store = res.get::<MemoryStore, _>("store")?;

        // Load or initialize negotiation state
        let mut state = match store.get::<NegotiationState>("negotiation_state") {
            Ok(s) => {
                println!(
                    "Seller: Round {} - Current offer on table: ${}",
                    s.round, s.current_offer
                );
                s
            }
            Err(_) => {
                // First round - initialize negotiation
                let initial_price = 10000;
                let buyer_budget = 1000;
                let s = NegotiationState::new(initial_price, buyer_budget);

                println!("Seller: Starting negotiation!");
                println!("Seller: Initial asking price: ${initial_price}");
                println!("Buyer budget: ${buyer_budget}");
                println!("{}", "=".repeat(50));
                s
            }
        };

        // Calculate new offer (unless it's the first round)
        if state.round > 1 {
            let reduction = Self::calculate_price_reduction();
            let new_offer = state.current_offer.saturating_sub(reduction);
            let minimum_price = 100;
            state.current_offer = std::cmp::max(new_offer, minimum_price);

            println!(
                "Seller: Round {} - Reducing price by ${}",
                state.round, reduction
            );
            println!("Seller: New offer: ${}", state.current_offer);

            if state.current_offer == minimum_price {
                println!("Seller: This is my final offer! Can't go any lower.");
            }
        } else {
            println!("Seller: My asking price is ${}", state.current_offer);
        }

        // Store the current negotiation state
        store.put("negotiation_state", state)?;
        println!("Seller: Waiting for buyer's response...");
        println!("{}", "-".repeat(30));

        Ok(TaskResult::Single(NegotiationAction::BuyerEvaluate))
    }
}

/// Buyer task: evaluates offers and makes decisions
#[derive(Clone)]
struct BuyerTask;

impl BuyerTask {
    fn new() -> Self {
        Self
    }

    /// Evaluate if the offer is acceptable based on budget and negotiation strategy
    fn evaluate_offer(state: &NegotiationState) -> bool {
        let offer_ratio = state.current_offer as f64 / state.buyer_budget as f64;

        // Accept if offer is at or below budget
        if state.current_offer <= state.buyer_budget {
            return true;
        }

        // Give up if the offer is still way too high after many rounds
        if state.round >= 10 && offer_ratio > 3.0 {
            return false; // Will result in NoDeal
        }

        false
    }
}

#[task(state = NegotiationAction)]
impl BuyerTask {
    fn config(&self) -> TaskConfig {
        TaskConfig::minimal()
    }

    async fn run(&self, res: &Resources) -> Result<TaskResult<NegotiationAction>, CanoError> {
        let store = res.get::<MemoryStore, _>("store")?;

        let mut state: NegotiationState = store.get("negotiation_state").map_err(|e| {
            CanoError::task_execution(format!("Failed to load negotiation state: {e}"))
        })?;

        println!(
            "Buyer: Evaluating seller's offer of ${}",
            state.current_offer
        );

        let acceptable = Self::evaluate_offer(&state);

        if acceptable {
            if state.current_offer <= state.buyer_budget {
                println!(
                    "Buyer: Great! This offer (${}) fits my budget (${})",
                    state.current_offer, state.buyer_budget
                );
                println!("Buyer: I accept this deal!");
            }
        } else {
            let offer_ratio = state.current_offer as f64 / state.buyer_budget as f64;

            if state.round >= 10 && offer_ratio > 3.0 {
                println!(
                    "Buyer: This is taking too long and the offer (${}) is still {}x my budget.",
                    state.current_offer,
                    offer_ratio.round() as u32
                );
                println!("Buyer: I'm walking away from this negotiation.");
            } else {
                println!(
                    "Buyer: ${} is still above my budget of ${}.",
                    state.current_offer, state.buyer_budget
                );
                println!("Buyer: Can you do better?");
            }
        }

        if acceptable && state.current_offer <= state.buyer_budget {
            store.put("final_deal", state.clone())?;
            store.remove("negotiation_state")?;
            println!("Deal reached in round {}!", state.round);
            return Ok(TaskResult::Single(NegotiationAction::Deal));
        }

        // Check if we should give up
        let offer_ratio = state.current_offer as f64 / state.buyer_budget as f64;
        if state.round >= 10 && offer_ratio > 3.0 {
            store.put("failed_negotiation", state)?;
            store.remove("negotiation_state")?;
            return Ok(TaskResult::Single(NegotiationAction::NoDeal));
        }

        // Continue negotiation - increment round and go back to seller
        state.round += 1;
        store.put("negotiation_state", state)?;
        println!("{}", "-".repeat(30));

        Ok(TaskResult::Single(NegotiationAction::StartSelling))
    }
}

/// Negotiation orchestrator using Workflow
async fn run_negotiation_workflow() -> Result<(), CanoError> {
    println!("Starting Negotiation Workflow");
    println!("================================");
    println!("Seller starts at $10,000");
    println!("Buyer has a budget of $1,000");
    println!("Let's see if they can make a deal!");
    println!();

    let store = MemoryStore::new();

    // Create a Workflow that handles the negotiation process
    let workflow = Workflow::new(Resources::new().insert("store", store.clone()))
        .register(NegotiationAction::StartSelling, SellerTask::new())
        .register(NegotiationAction::BuyerEvaluate, BuyerTask::new())
        .add_exit_states(vec![
            NegotiationAction::Deal,
            NegotiationAction::NoDeal,
            NegotiationAction::Error,
        ]);

    // Execute the negotiation workflow
    match workflow.orchestrate(NegotiationAction::StartSelling).await {
        Ok(final_state) => {
            println!("{}", "=".repeat(50));

            match final_state {
                NegotiationAction::Deal => {
                    println!("NEGOTIATION SUCCESSFUL!");

                    if let Ok(deal) = store.get::<NegotiationState>("final_deal") {
                        println!("Final Deal Summary:");
                        println!("  Final price: ${}", deal.current_offer);
                        println!("  Buyer budget: ${}", deal.buyer_budget);
                        println!("  Rounds of negotiation: {}", deal.round);
                        println!(
                            "  Savings from initial price: ${}",
                            deal.seller_initial_price - deal.current_offer
                        );

                        let savings_percent = ((deal.seller_initial_price - deal.current_offer)
                            as f64
                            / deal.seller_initial_price as f64)
                            * 100.0;
                        println!("  Discount achieved: {savings_percent:.1}%");
                    }
                }
                NegotiationAction::NoDeal => {
                    println!("NEGOTIATION FAILED!");

                    if let Ok(failed) = store.get::<NegotiationState>("failed_negotiation") {
                        println!("Negotiation Summary:");
                        println!("  Final offer: ${}", failed.current_offer);
                        println!("  Buyer budget: ${}", failed.buyer_budget);
                        println!("  Rounds attempted: {}", failed.round);
                        println!(
                            "  Gap remaining: ${}",
                            failed.current_offer - failed.buyer_budget
                        );

                        let gap_ratio = failed.current_offer as f64 / failed.buyer_budget as f64;
                        println!("  Offer was {gap_ratio:.1}x the buyer's budget");
                    }

                    println!("The buyer walked away - no deal was reached.");
                }
                NegotiationAction::Error => {
                    eprintln!("Negotiation terminated due to an error");
                    return Err(CanoError::workflow(
                        "Negotiation terminated with error state",
                    ));
                }
                other => {
                    eprintln!("Negotiation ended in unexpected state: {other:?}");
                    return Err(CanoError::workflow(format!(
                        "Negotiation ended in unexpected state: {other:?}"
                    )));
                }
            }
        }
        Err(e) => {
            eprintln!("Negotiation workflow failed: {e}");
            return Err(e);
        }
    }

    Ok(())
}

#[tokio::main]
async fn main() {
    println!("Negotiation Workflow Example");
    println!("===============================");

    match run_negotiation_workflow().await {
        Ok(()) => {
            println!("\nNegotiation workflow completed!");
        }
        Err(e) => {
            eprintln!("\nNegotiation workflow failed: {e}");
            std::process::exit(1);
        }
    }

    println!("\nThis example demonstrates:");
    println!("  Inter-task communication via shared store");
    println!("  Iterative workflow with conditional routing");
    println!("  Random business logic (price reductions)");
    println!("  Multiple exit conditions (deal/no deal)");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_seller_task_initialization() {
        let seller = SellerTask::new();
        let store = MemoryStore::new();
        let res = Resources::new().insert("store", store.clone());

        // First run should initialize the negotiation
        let result = seller.run(&res).await.unwrap();
        assert_eq!(result, TaskResult::Single(NegotiationAction::BuyerEvaluate));

        // Verify state was stored
        let state: NegotiationState = store.get("negotiation_state").unwrap();
        assert_eq!(state.current_offer, 10000);
        assert_eq!(state.buyer_budget, 1000);
        assert_eq!(state.round, 1);
    }

    #[tokio::test]
    async fn test_buyer_task_evaluation() {
        let store = MemoryStore::new();

        // Setup: affordable offer
        let affordable_state = NegotiationState::new(10000, 1000);
        let mut affordable_state_modified = affordable_state.clone();
        affordable_state_modified.current_offer = 800; // Within budget
        store
            .put("negotiation_state", affordable_state_modified.clone())
            .unwrap();

        let buyer = BuyerTask::new();
        let res = Resources::new().insert("store", store.clone());
        let result = buyer.run(&res).await.unwrap();

        // Should accept the deal
        assert_eq!(result, TaskResult::Single(NegotiationAction::Deal));
    }

    #[tokio::test]
    async fn test_buyer_task_rejection() {
        let store = MemoryStore::new();

        // Setup: expensive offer, early round
        let expensive_state = NegotiationState::new(10000, 1000);
        let mut expensive_state_modified = expensive_state.clone();
        expensive_state_modified.current_offer = 5000; // Way above budget
        expensive_state_modified.round = 2; // Early round
        store
            .put("negotiation_state", expensive_state_modified)
            .unwrap();

        let buyer = BuyerTask::new();
        let res = Resources::new().insert("store", store.clone());
        let result = buyer.run(&res).await.unwrap();

        // Should continue negotiating
        assert_eq!(result, TaskResult::Single(NegotiationAction::StartSelling));
    }

    #[tokio::test]
    async fn test_buyer_task_gives_up() {
        let store = MemoryStore::new();

        // Setup: expensive offer, many rounds
        let expensive_state = NegotiationState::new(10000, 1000);
        let mut expensive_state_modified = expensive_state.clone();
        expensive_state_modified.current_offer = 5000; // Still way above budget
        expensive_state_modified.round = 10; // Many rounds
        store
            .put("negotiation_state", expensive_state_modified)
            .unwrap();

        let buyer = BuyerTask::new();
        let res = Resources::new().insert("store", store.clone());
        let result = buyer.run(&res).await.unwrap();

        // Should give up
        assert_eq!(result, TaskResult::Single(NegotiationAction::NoDeal));
    }

    #[tokio::test]
    async fn test_seller_price_reduction() {
        let store = MemoryStore::new();

        // Setup: existing negotiation state
        let initial_state = NegotiationState::new(10000, 1000);
        let mut ongoing_state = initial_state.clone();
        ongoing_state.round = 2; // Not first round
        ongoing_state.current_offer = 8000;
        store
            .put("negotiation_state", ongoing_state.clone())
            .unwrap();

        let seller = SellerTask::new();
        let res = Resources::new().insert("store", store.clone());
        let result = seller.run(&res).await.unwrap();

        assert_eq!(result, TaskResult::Single(NegotiationAction::BuyerEvaluate));

        // Verify price was reduced
        let updated_state: NegotiationState = store.get("negotiation_state").unwrap();
        assert!(updated_state.current_offer < ongoing_state.current_offer);
        assert!(updated_state.current_offer >= 100); // Minimum price
    }

    #[tokio::test]
    async fn test_negotiation_state_structure() {
        let state = NegotiationState::new(5000, 2000);

        assert_eq!(state.current_offer, 5000);
        assert_eq!(state.buyer_budget, 2000);
        assert_eq!(state.round, 1);
        assert_eq!(state.seller_initial_price, 5000);
    }

    #[tokio::test]
    async fn test_price_reduction_range() {
        // Test the price reduction range multiple times
        for _ in 0..10 {
            let reduction = SellerTask::calculate_price_reduction();
            assert!(reduction >= 500);
            assert!(reduction <= 2000);
        }
    }

    #[tokio::test]
    async fn test_full_negotiation_workflow() {
        // This will run the full workflow - may result in either deal or no deal
        let result = run_negotiation_workflow().await;

        // The workflow should complete without errors, regardless of outcome
        assert!(result.is_ok());
    }
}
