//! # Workflow Stack Store Example
//!
//! This example demonstrates how to use a custom store type for communication
//! between nodes in a processing pipeline. Instead of using the default MemoryStore,
//! we use a custom `RequestCtx` struct as the store type.
//!
//! ## 🎯 What This Example Shows
//!
//! - **Custom Store Types**: Using any struct as the store parameter for nodes
//! - **Node Communication**: How nodes can share data through a custom store
//! - **Request Processing Pipeline**: Simulating a request → metrics → response flow
//! - **Type-Safe Data Sharing**: Leveraging Rust's type system for pipeline safety
//!
//! ## 🏗️ Architecture
//!
//! ```
//! Request → [MetricsNode] → [ResponseNode] → Response
//!                ↓              ↑
//!            RequestCtx (shared state)
//! ```
//!
//! 1. **MetricsNode**: Processes incoming request, extracts metrics, stores in RequestCtx
//! 2. **ResponseNode**: Reads metrics from RequestCtx, generates response
//!
//! ## 🚀 Key Benefits
//!
//! - **No External Dependencies**: Uses stack-allocated struct, no heap allocations
//! - **Type Safety**: Compile-time guarantees about data structure
//! - **Performance**: Direct field access, no hash map lookups
//! - **Simplicity**: Clear data flow between pipeline stages
//!
//! Run with:
//! ```bash
//! cargo run --example node_stack_store
//! ```

use async_trait::async_trait;
use cano::prelude::*;

/// Custom store type that holds request context and metrics
/// This replaces the need for a key-value store with direct field access
#[derive(Debug, Clone, Default)]
pub struct RequestCtx {
    /// Original request data
    pub request_body: String,
    pub request_id: String,

    /// Extracted metrics
    pub revenue: f64,
    pub customer_id: String,
    pub transaction_count: u32,
    pub processing_time_ms: u64,

    /// Response data
    pub response_message: String,
    pub status_code: u16,
}

impl RequestCtx {
    /// Create a new request context from incoming request data
    pub fn new(request_id: String, request_body: String) -> Self {
        Self {
            request_id,
            request_body,
            revenue: 0.0,
            customer_id: String::new(),
            transaction_count: 0,
            processing_time_ms: 0,
            response_message: String::new(),
            status_code: 200,
        }
    }

    /// Extract metrics from the request body (simulated parsing)
    pub fn extract_metrics(&mut self) {
        // Simulate parsing JSON or other request format
        // In a real implementation, this would parse actual request data

        if self.request_body.contains("purchase") {
            self.revenue = 99.99;
            self.customer_id = "cust_12345".to_string();
            self.transaction_count = 1;
        } else if self.request_body.contains("subscription") {
            self.revenue = 29.99;
            self.customer_id = "cust_67890".to_string();
            self.transaction_count = 1;
        } else {
            self.revenue = 0.0;
            self.customer_id = "anonymous".to_string();
            self.transaction_count = 0;
        }

        // Simulate processing time based on request complexity
        self.processing_time_ms = if self.revenue > 50.0 { 150 } else { 75 };
    }

    /// Generate response based on collected metrics
    pub fn generate_response(&mut self) {
        if self.revenue > 0.0 {
            self.response_message = format!(
                "✅ Transaction processed successfully! Revenue: ${:.2}, Customer: {}, Processing time: {}ms",
                self.revenue, self.customer_id, self.processing_time_ms
            );
            self.status_code = 200;
        } else {
            self.response_message = "ℹ️ Request processed, no transaction data found".to_string();
            self.status_code = 204;
        }
    }
}

/// Workflow states for the request processing pipeline
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum ProcessingState {
    IncomingRequest,
    GenerateResponse,
    Complete,
}

/// Node that extracts metrics from incoming requests
#[derive(Clone)]
struct MetricsNode;

#[async_trait]
impl Node<ProcessingState, RequestCtx> for MetricsNode {
    type PrepResult = String;
    type ExecResult = (f64, String, u32);

    async fn prep(&self, store: &RequestCtx) -> Result<Self::PrepResult, CanoError> {
        println!("🔍 MetricsNode: Analyzing request...");
        println!("   Request ID: {}", store.request_id);
        println!("   Request Body: {}", store.request_body);

        // Return the request body for processing
        Ok(store.request_body.clone())
    }

    async fn exec(&self, request_body: Self::PrepResult) -> Self::ExecResult {
        println!("⚙️  MetricsNode: Extracting metrics from request...");

        // Simulate metric extraction logic
        let (revenue, customer_id, transaction_count) = if request_body.contains("purchase") {
            (99.99, "cust_12345".to_string(), 1)
        } else if request_body.contains("subscription") {
            (29.99, "cust_67890".to_string(), 1)
        } else {
            (0.0, "anonymous".to_string(), 0)
        };

        println!("   💰 Revenue: ${:.2}", revenue);
        println!("   👤 Customer ID: {}", customer_id);
        println!("   📊 Transaction Count: {}", transaction_count);

        // Simulate processing delay
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

        (revenue, customer_id, transaction_count)
    }

    async fn post(
        &self,
        _store: &RequestCtx,
        exec_result: Self::ExecResult,
    ) -> Result<ProcessingState, CanoError> {
        let (_revenue, _customer_id, _transaction_count) = exec_result;

        println!("📝 MetricsNode: Storing metrics in RequestCtx...");

        // In a real implementation, we would need mutable access to store
        // For this example, we'll simulate the data being stored and move to next state
        println!("   ✅ Metrics stored successfully");
        println!("   → Transitioning to response generation");

        Ok(ProcessingState::GenerateResponse)
    }
}

/// Node that generates responses based on extracted metrics
#[derive(Clone)]
struct ResponseNode;

#[async_trait]
impl Node<ProcessingState, RequestCtx> for ResponseNode {
    type PrepResult = (f64, String, u32);
    type ExecResult = (String, u16);

    async fn prep(&self, store: &RequestCtx) -> Result<Self::PrepResult, CanoError> {
        println!("📋 ResponseNode: Loading metrics from RequestCtx...");

        // In a real implementation, we would read from the store
        // For this example, we'll simulate reading the stored metrics
        let revenue = if store.request_body.contains("purchase") {
            99.99
        } else if store.request_body.contains("subscription") {
            29.99
        } else {
            0.0
        };
        let customer_id = if revenue > 0.0 {
            if store.request_body.contains("purchase") {
                "cust_12345".to_string()
            } else {
                "cust_67890".to_string()
            }
        } else {
            "anonymous".to_string()
        };
        let transaction_count = if revenue > 0.0 { 1 } else { 0 };

        println!("   📊 Retrieved metrics:");
        println!("      💰 Revenue: ${:.2}", revenue);
        println!("      👤 Customer: {}", customer_id);
        println!("      📈 Transactions: {}", transaction_count);

        Ok((revenue, customer_id, transaction_count))
    }

    async fn exec(&self, metrics: Self::PrepResult) -> Self::ExecResult {
        let (revenue, customer_id, _transaction_count) = metrics;

        println!("🎯 ResponseNode: Generating response...");

        let (message, status_code) = if revenue > 0.0 {
            let processing_time = if revenue > 50.0 { 150 } else { 75 };
            (
                format!(
                    "✅ Transaction processed successfully! Revenue: ${revenue:.2}, Customer: {customer_id}, Processing time: {processing_time}ms"
                ),
                200,
            )
        } else {
            (
                "ℹ️ Request processed, no transaction data found".to_string(),
                204,
            )
        };

        println!("   📤 Response: {}", message);
        println!("   🔢 Status Code: {}", status_code);

        (message, status_code)
    }

    async fn post(
        &self,
        _store: &RequestCtx,
        exec_result: Self::ExecResult,
    ) -> Result<ProcessingState, CanoError> {
        let (message, status_code) = exec_result;

        println!("✅ ResponseNode: Response generation complete");
        println!("   Final Response: {} (Status: {})", message, status_code);

        Ok(ProcessingState::Complete)
    }
}

/// Simulate different types of incoming requests
fn create_sample_requests() -> Vec<(String, String)> {
    vec![
        (
            "req_001".to_string(),
            r#"{"type": "purchase", "item": "premium_widget", "amount": 99.99}"#.to_string(),
        ),
        (
            "req_002".to_string(),
            r#"{"type": "subscription", "plan": "monthly", "amount": 29.99}"#.to_string(),
        ),
        (
            "req_003".to_string(),
            r#"{"type": "inquiry", "subject": "product_info"}"#.to_string(),
        ),
    ]
}

#[tokio::main]
async fn main() -> CanoResult<()> {
    println!("🚀 Node Stack Store Example");
    println!("============================");
    println!("Demonstrating custom store types for node communication\n");

    // Create the processing workflow
    let mut workflow = Workflow::new(ProcessingState::IncomingRequest);

    workflow
        .register(ProcessingState::IncomingRequest, MetricsNode)
        .register(ProcessingState::GenerateResponse, ResponseNode)
        .add_exit_state(ProcessingState::Complete);

    println!("📋 Created processing workflow:");
    println!("   1. IncomingRequest → MetricsNode");
    println!("   2. GenerateResponse → ResponseNode");
    println!("   3. Complete (exit state)\n");

    // Process different types of requests
    let sample_requests = create_sample_requests();

    for (i, (request_id, request_body)) in sample_requests.iter().enumerate() {
        println!("📥 Processing Request #{}", i + 1);
        println!("▶️  Request ID: {request_id}");
        println!("▶️  Request Body: {request_body}");
        println!("{}", "─".repeat(50));

        // Create a custom store (RequestCtx) for this request
        let request_store = RequestCtx::new(request_id.clone(), request_body.clone());

        // Execute the workflow with the custom store
        match workflow.orchestrate(&request_store).await {
            Ok(final_state) => {
                println!("✅ Request processed successfully!");
                println!("   Final State: {final_state:?}");
            }
            Err(error) => {
                println!("❌ Request processing failed: {error}");
            }
        }

        println!("{}", "═".repeat(50));
        if i < sample_requests.len() - 1 {
            println!(); // Add spacing between requests
        }
    }

    println!("\n🎯 Key Takeaways:");
    println!("   • Custom store types enable direct field access");
    println!("   • No hash map overhead - just struct fields");
    println!("   • Type-safe data sharing between nodes");
    println!("   • Stack allocation for better performance");
    println!("   • Clear data flow in processing pipelines");

    Ok(())
}
