//! # Workflow with Shared Store Example
//!
//! This example demonstrates how to use the MemoryStore for communication
//! between tasks in a processing pipeline. The store acts as shared state
//! that flows through the workflow.
//!
//! ## What This Example Shows
//!
//! - **Shared State**: Using MemoryStore to share data between workflow tasks
//! - **Task Communication**: How tasks read and write to the shared store
//! - **Request Processing Pipeline**: Simulating a request -> metrics -> response flow
//! - **Type-Safe Data Sharing**: Leveraging the store's type-safe get/put operations
//!
//! ## Architecture
//!
//! ```
//! Request -> [MetricsTask] -> [ResponseTask] -> Response
//!                ↓                ↑
//!            MemoryStore (shared state)
//! ```
//!
//! 1. **MetricsTask**: Processes incoming request, extracts metrics, stores in MemoryStore
//! 2. **ResponseTask**: Reads metrics from MemoryStore, generates response
//!
//! Run with:
//! ```bash
//! cargo run --example workflow_stack_store
//! ```

use cano::prelude::*;
use std::time::Duration;

/// Workflow states for request processing
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum RequestState {
    Start,
    MetricsExtracted,
    ResponseGenerated,
    Complete,
}

/// Metrics task: extracts metrics from request and stores them
#[derive(Clone)]
pub struct MetricsTask;

#[task(state = RequestState)]
impl MetricsTask {
    async fn run(&self, res: &Resources) -> Result<TaskResult<RequestState>, CanoError> {
        let store = res.get::<MemoryStore, _>("store")?;

        // Get request data from store
        let request_id: String = store.get("request_id")?;
        let request_body: String = store.get("request_body")?;
        println!("MetricsTask: Processing request {}", request_id);

        // Simulate processing time
        tokio::time::sleep(Duration::from_millis(50)).await;

        // Extract metrics based on request content
        let (revenue, customer_id, transaction_count) = if request_body.contains("purchase") {
            (99.99, "cust_12345".to_string(), 1u32)
        } else if request_body.contains("subscription") {
            (29.99, "cust_67890".to_string(), 1u32)
        } else {
            (0.0, "anonymous".to_string(), 0u32)
        };

        println!(
            "MetricsTask: Extracted metrics - Revenue: ${:.2}, Customer: {}, Transactions: {}",
            revenue, customer_id, transaction_count
        );

        // Store metrics in the shared store
        store.put("revenue", revenue)?;
        store.put("customer_id", customer_id)?;
        store.put("transaction_count", transaction_count)?;

        let processing_time_ms = if revenue > 50.0 { 150u64 } else { 75u64 };
        store.put("processing_time_ms", processing_time_ms)?;

        println!("MetricsTask: Stored metrics in shared store");
        Ok(TaskResult::Single(RequestState::MetricsExtracted))
    }
}

/// Response task: generates response based on metrics
#[derive(Clone)]
pub struct ResponseTask;

#[task(state = RequestState)]
impl ResponseTask {
    async fn run(&self, res: &Resources) -> Result<TaskResult<RequestState>, CanoError> {
        let store = res.get::<MemoryStore, _>("store")?;

        // Read metrics from shared store
        let revenue: f64 = store.get("revenue")?;
        let customer_id: String = store.get("customer_id")?;
        let transaction_count: u32 = store.get("transaction_count")?;
        let processing_time_ms: u64 = store.get("processing_time_ms")?;
        println!("ResponseTask: Read metrics from shared store");

        // Simulate response generation time
        tokio::time::sleep(Duration::from_millis(30)).await;
        println!("ResponseTask: Generating response");

        // Generate response based on metrics
        let (response_message, status_code) = if revenue > 0.0 {
            let message = format!(
                "Transaction successful! Customer {} processed {} transaction(s) totaling ${:.2}. Processing time: {}ms",
                customer_id, transaction_count, revenue, processing_time_ms
            );
            (message, 200u16)
        } else {
            let message = format!(
                "No transactions processed for customer {}. Processing time: {}ms",
                customer_id, processing_time_ms
            );
            (message, 200u16)
        };

        println!(
            "ResponseTask: Generated response with status {}",
            status_code
        );

        // Store response in shared store
        store.put("response_message", response_message.clone())?;
        store.put("status_code", status_code)?;

        println!("ResponseTask: Stored response in shared store");
        println!("Response: {}", response_message);

        Ok(TaskResult::Single(RequestState::Complete))
    }
}

#[tokio::main]
async fn main() -> CanoResult<()> {
    println!("Workflow Stack Store Example");
    println!("=================================\n");
    println!("This example demonstrates how tasks communicate through MemoryStore\n");

    // Example 1: Purchase request
    {
        println!("Example 1: Purchase Request");
        println!("-------------------------------");

        let store = MemoryStore::new();

        // Initialize request data in store
        store.put("request_id", "req_001".to_string())?;
        store.put("request_body", "purchase: laptop".to_string())?;

        // Create workflow
        let workflow = Workflow::new(Resources::new().insert("store", store.clone()))
            .register(RequestState::Start, MetricsTask)
            .register(RequestState::MetricsExtracted, ResponseTask)
            .add_exit_state(RequestState::Complete);

        // Execute workflow
        println!("\nStarting workflow...\n");
        let final_state = workflow.orchestrate(RequestState::Start).await?;

        // Display results
        println!("\nFinal Results:");
        println!("  State: {:?}", final_state);
        println!(
            "  Revenue: ${:.2}",
            store.get::<f64>("revenue").unwrap_or(0.0)
        );
        println!(
            "  Customer: {}",
            store.get::<String>("customer_id").unwrap_or_default()
        );
        println!(
            "  Response: {}",
            store.get::<String>("response_message").unwrap_or_default()
        );
        println!();
    }

    // Example 2: Subscription request
    {
        println!("Example 2: Subscription Request");
        println!("-----------------------------------");

        let store = MemoryStore::new();

        // Initialize request data
        store.put("request_id", "req_002".to_string())?;
        store.put("request_body", "subscription: premium plan".to_string())?;

        // Create workflow
        let workflow = Workflow::new(Resources::new().insert("store", store.clone()))
            .register(RequestState::Start, MetricsTask)
            .register(RequestState::MetricsExtracted, ResponseTask)
            .add_exit_state(RequestState::Complete);

        // Execute workflow
        println!("\nStarting workflow...\n");
        let final_state = workflow.orchestrate(RequestState::Start).await?;

        // Display results
        println!("\nFinal Results:");
        println!("  State: {:?}", final_state);
        println!(
            "  Revenue: ${:.2}",
            store.get::<f64>("revenue").unwrap_or(0.0)
        );
        println!(
            "  Customer: {}",
            store.get::<String>("customer_id").unwrap_or_default()
        );
        println!(
            "  Response: {}",
            store.get::<String>("response_message").unwrap_or_default()
        );
        println!();
    }

    // Example 3: Unknown request type
    {
        println!("Example 3: Unknown Request Type");
        println!("-----------------------------------");

        let store = MemoryStore::new();

        // Initialize request data
        store.put("request_id", "req_003".to_string())?;
        store.put("request_body", "query: account balance".to_string())?;

        // Create workflow
        let workflow = Workflow::new(Resources::new().insert("store", store.clone()))
            .register(RequestState::Start, MetricsTask)
            .register(RequestState::MetricsExtracted, ResponseTask)
            .add_exit_state(RequestState::Complete);

        // Execute workflow
        println!("\nStarting workflow...\n");
        let final_state = workflow.orchestrate(RequestState::Start).await?;

        // Display results
        println!("\nFinal Results:");
        println!("  State: {:?}", final_state);
        println!(
            "  Revenue: ${:.2}",
            store.get::<f64>("revenue").unwrap_or(0.0)
        );
        println!(
            "  Customer: {}",
            store.get::<String>("customer_id").unwrap_or_default()
        );
        println!(
            "  Response: {}",
            store.get::<String>("response_message").unwrap_or_default()
        );
        println!();
    }

    println!("Workflow Stack Store example completed successfully!");
    println!("\nKey Takeaways:");
    println!("  MemoryStore provides shared state between workflow tasks");
    println!("  Tasks can read data stored by previous tasks");
    println!("  Type-safe get/put operations ensure data consistency");
    println!("  Clean separation of concerns between tasks");

    Ok(())
}
