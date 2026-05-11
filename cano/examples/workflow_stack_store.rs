//! # Workflow with Shared Store Example
//!
//! This example demonstrates how to use the MemoryStore for communication
//! between nodes in a processing pipeline. The store acts as shared state
//! that flows through the workflow.
//!
//! ## 🎯 What This Example Shows
//!
//! - **Shared State**: Using MemoryStore to share data between workflow nodes
//! - **Node Communication**: How nodes read and write to the shared store
//! - **Request Processing Pipeline**: Simulating a request → metrics → response flow
//! - **Type-Safe Data Sharing**: Leveraging the store's type-safe get/put operations
//!
//! ## 🏗️ Architecture
//!
//! ```
//! Request → [MetricsNode] → [ResponseNode] → Response
//!                ↓              ↑
//!            MemoryStore (shared state)
//! ```
//!
//! 1. **MetricsNode**: Processes incoming request, extracts metrics, stores in MemoryStore
//! 2. **ResponseNode**: Reads metrics from MemoryStore, generates response
//!
//! ## 🚀 Key Benefits
//!
//! - **Simple Communication**: Nodes communicate through the shared store
//! - **Type Safety**: Compile-time guarantees about data types
//! - **Clean Separation**: Each node has a clear responsibility
//! - **Testability**: Easy to test nodes in isolation
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

/// Metrics Node: Extracts metrics from request and stores them
#[derive(Clone)]
pub struct MetricsNode;

#[task::node(state = RequestState)]
impl MetricsNode {
    type PrepResult = (String, String);
    type ExecResult = (f64, String, u32);

    async fn prep(&self, res: &Resources) -> Result<Self::PrepResult, CanoError> {
        let store = res.get::<MemoryStore, _>("store")?;
        // Get request data from store
        let request_id: String = store.get("request_id")?;
        let request_body: String = store.get("request_body")?;

        println!("📊 MetricsNode: Processing request {}", request_id);
        Ok((request_id, request_body))
    }

    async fn exec(&self, (request_id, request_body): Self::PrepResult) -> Self::ExecResult {
        println!(
            "🔍 MetricsNode: Extracting metrics from request {}",
            request_id
        );

        // Simulate processing time
        tokio::time::sleep(Duration::from_millis(50)).await;

        // Extract metrics based on request content
        let (revenue, customer_id, transaction_count) = if request_body.contains("purchase") {
            (99.99, "cust_12345".to_string(), 1)
        } else if request_body.contains("subscription") {
            (29.99, "cust_67890".to_string(), 1)
        } else {
            (0.0, "anonymous".to_string(), 0)
        };

        println!(
            "💰 MetricsNode: Extracted metrics - Revenue: ${:.2}, Customer: {}, Transactions: {}",
            revenue, customer_id, transaction_count
        );

        (revenue, customer_id, transaction_count)
    }

    async fn post(
        &self,
        res: &Resources,
        (revenue, customer_id, transaction_count): Self::ExecResult,
    ) -> Result<RequestState, CanoError> {
        let store = res.get::<MemoryStore, _>("store")?;
        // Store metrics in the shared store
        store.put("revenue", revenue)?;
        store.put("customer_id", customer_id)?;
        store.put("transaction_count", transaction_count)?;

        // Calculate and store processing time
        let processing_time_ms = if revenue > 50.0 { 150u64 } else { 75u64 };
        store.put("processing_time_ms", processing_time_ms)?;

        println!("✅ MetricsNode: Stored metrics in shared store");
        Ok(RequestState::MetricsExtracted)
    }
}

/// Response Node: Generates response based on metrics
#[derive(Clone)]
pub struct ResponseNode;

#[task::node(state = RequestState)]
impl ResponseNode {
    type PrepResult = (f64, String, u32, u64);
    type ExecResult = (String, u16);

    async fn prep(&self, res: &Resources) -> Result<Self::PrepResult, CanoError> {
        let store = res.get::<MemoryStore, _>("store")?;
        // Read metrics from shared store
        let revenue: f64 = store.get("revenue")?;
        let customer_id: String = store.get("customer_id")?;
        let transaction_count: u32 = store.get("transaction_count")?;
        let processing_time_ms: u64 = store.get("processing_time_ms")?;

        println!("📖 ResponseNode: Read metrics from shared store");
        Ok((revenue, customer_id, transaction_count, processing_time_ms))
    }

    async fn exec(
        &self,
        (revenue, customer_id, transaction_count, processing_time_ms): Self::PrepResult,
    ) -> Self::ExecResult {
        println!("✏️  ResponseNode: Generating response");

        // Simulate response generation time
        tokio::time::sleep(Duration::from_millis(30)).await;

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
            "📝 ResponseNode: Generated response with status {}",
            status_code
        );
        (response_message, status_code)
    }

    async fn post(
        &self,
        res: &Resources,
        (response_message, status_code): Self::ExecResult,
    ) -> Result<RequestState, CanoError> {
        let store = res.get::<MemoryStore, _>("store")?;
        // Store response in shared store
        store.put("response_message", response_message.clone())?;
        store.put("status_code", status_code)?;

        println!("✅ ResponseNode: Stored response in shared store");
        println!("📤 Response: {}", response_message);

        Ok(RequestState::Complete)
    }
}

#[tokio::main]
async fn main() -> CanoResult<()> {
    println!("🚀 Workflow Stack Store Example");
    println!("=================================\n");
    println!("This example demonstrates how nodes communicate through MemoryStore\n");

    // Example 1: Purchase request
    {
        println!("📋 Example 1: Purchase Request");
        println!("-------------------------------");

        let store = MemoryStore::new();

        // Initialize request data in store
        store.put("request_id", "req_001".to_string())?;
        store.put("request_body", "purchase: laptop".to_string())?;

        // Create workflow
        let workflow = Workflow::new(Resources::new().insert("store", store.clone()))
            .register(RequestState::Start, MetricsNode)
            .register(RequestState::MetricsExtracted, ResponseNode)
            .add_exit_state(RequestState::Complete);

        // Execute workflow
        println!("\n🎬 Starting workflow...\n");
        let final_state = workflow.orchestrate(RequestState::Start).await?;

        // Display results
        println!("\n📊 Final Results:");
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
        println!("📋 Example 2: Subscription Request");
        println!("-----------------------------------");

        let store = MemoryStore::new();

        // Initialize request data
        store.put("request_id", "req_002".to_string())?;
        store.put("request_body", "subscription: premium plan".to_string())?;

        // Create workflow
        let workflow = Workflow::new(Resources::new().insert("store", store.clone()))
            .register(RequestState::Start, MetricsNode)
            .register(RequestState::MetricsExtracted, ResponseNode)
            .add_exit_state(RequestState::Complete);

        // Execute workflow
        println!("\n🎬 Starting workflow...\n");
        let final_state = workflow.orchestrate(RequestState::Start).await?;

        // Display results
        println!("\n📊 Final Results:");
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
        println!("📋 Example 3: Unknown Request Type");
        println!("-----------------------------------");

        let store = MemoryStore::new();

        // Initialize request data
        store.put("request_id", "req_003".to_string())?;
        store.put("request_body", "query: account balance".to_string())?;

        // Create workflow
        let workflow = Workflow::new(Resources::new().insert("store", store.clone()))
            .register(RequestState::Start, MetricsNode)
            .register(RequestState::MetricsExtracted, ResponseNode)
            .add_exit_state(RequestState::Complete);

        // Execute workflow
        println!("\n🎬 Starting workflow...\n");
        let final_state = workflow.orchestrate(RequestState::Start).await?;

        // Display results
        println!("\n📊 Final Results:");
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

    println!("✅ Workflow Stack Store example completed successfully!");
    println!("\n💡 Key Takeaways:");
    println!("  • MemoryStore provides shared state between workflow nodes");
    println!("  • Nodes can read data stored by previous nodes");
    println!("  • Type-safe get/put operations ensure data consistency");
    println!("  • Clean separation of concerns between nodes");

    Ok(())
}
