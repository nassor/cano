//! # Workflow per HTTP Request Example
//!
//! This example demonstrates how to trigger a Cano workflow for every incoming
//! HTTP request (using Axum) and return the results in the response.
//!
//! ## Key Concepts
//!
//! - **Per-request store isolation**: Each request gets its own `MemoryStore`
//!   so concurrent requests never interfere with each other.
//! - **Workflow factory function**: The workflow structure is cheap to build —
//!   tasks are small `Clone` structs wrapped in `Arc` internally.
//! - **Data flow via store**: Request data is written to the store before
//!   orchestration; results are read from the store after it completes.
//!
//! ## Running
//!
//! ```bash
//! cargo run --example workflow_on_request
//! ```
//!
//! Then in another terminal:
//!
//! ```bash
//! curl -X POST http://localhost:3000/process \
//!   -H "Content-Type: application/json" \
//!   -d '{"text": "Hello World From Cano"}'
//! ```
//!
//! Expected response:
//!
//! ```json
//! {"original":"Hello World From Cano","word_count":4,"uppercased":"HELLO WORLD FROM CANO"}
//! ```

use async_trait::async_trait;
use axum::http::StatusCode;
use axum::routing::post;
use axum::{Json, Router};
use cano::prelude::*;
use serde::{Deserialize, Serialize};
use std::time::Duration;

// ============================================================================
// Workflow States
// ============================================================================

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum TextPipelineState {
    /// Parse and validate the incoming text
    Parse,
    /// Transform the text (uppercase, word count, etc.)
    Transform,
    /// Pipeline finished successfully
    Done,
}

// ============================================================================
// Request / Response types
// ============================================================================

#[derive(Deserialize)]
struct ProcessRequest {
    text: String,
}

#[derive(Serialize)]
struct ProcessResponse {
    original: String,
    word_count: usize,
    uppercased: String,
}

// ============================================================================
// Tasks
// ============================================================================

/// Reads "input_text" from the store, validates it is non-empty, and copies it
/// through so downstream tasks can consume it.
#[derive(Clone)]
struct ParseTask;

#[async_trait]
impl Task<TextPipelineState> for ParseTask {
    async fn run(&self, store: &MemoryStore) -> Result<TaskResult<TextPipelineState>, CanoError> {
        let text: String = store
            .get("input_text")
            .map_err(|e| CanoError::task_execution(format!("missing input_text: {e}")))?;

        if text.trim().is_empty() {
            return Err(CanoError::task_execution("input text is empty"));
        }

        // Store the validated text for the next stage
        store
            .put("validated_text", text)
            .map_err(|e| CanoError::store(format!("{e}")))?;

        Ok(TaskResult::Single(TextPipelineState::Transform))
    }
}

/// Transforms the validated text: computes word count and uppercased version,
/// then writes results back to the store.
#[derive(Clone)]
struct TransformTask;

#[async_trait]
impl Task<TextPipelineState> for TransformTask {
    async fn run(&self, store: &MemoryStore) -> Result<TaskResult<TextPipelineState>, CanoError> {
        let text: String = store
            .get("validated_text")
            .map_err(|e| CanoError::task_execution(format!("missing validated_text: {e}")))?;

        let word_count = text.split_whitespace().count();
        let uppercased = text.to_uppercase();

        store
            .put("word_count", word_count)
            .map_err(|e| CanoError::store(format!("{e}")))?;
        store
            .put("uppercased", uppercased)
            .map_err(|e| CanoError::store(format!("{e}")))?;

        Ok(TaskResult::Single(TextPipelineState::Done))
    }
}

// ============================================================================
// Workflow factory
// ============================================================================

/// Build a fresh workflow with its own store.
///
/// Because `Workflow::new` takes ownership of the store, we return both the
/// workflow and a clone of the store so the caller can read results afterwards.
fn build_workflow(store: MemoryStore) -> Workflow<TextPipelineState> {
    Workflow::new(store)
        .register(TextPipelineState::Parse, ParseTask)
        .register(TextPipelineState::Transform, TransformTask)
        .add_exit_state(TextPipelineState::Done)
        .with_timeout(Duration::from_secs(5))
}

// ============================================================================
// Axum handler
// ============================================================================

async fn process_handler(
    Json(payload): Json<ProcessRequest>,
) -> Result<Json<ProcessResponse>, StatusCode> {
    // Each request gets its own store — full isolation.
    let store = MemoryStore::new();

    // Inject request data into the store before running the workflow.
    store
        .put("input_text", payload.text.clone())
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let workflow = build_workflow(store.clone());

    // Run the FSM to completion.
    workflow
        .orchestrate(TextPipelineState::Parse)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    // Read the results that the tasks wrote to the store.
    let word_count: usize = store
        .get("word_count")
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let uppercased: String = store
        .get("uppercased")
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(Json(ProcessResponse {
        original: payload.text,
        word_count,
        uppercased,
    }))
}

// ============================================================================
// Main
// ============================================================================

#[tokio::main]
async fn main() {
    let app = Router::new().route("/process", post(process_handler));

    let listener = tokio::net::TcpListener::bind("0.0.0.0:4001")
        .await
        .expect("failed to bind to port 4001");

    println!("Listening on http://localhost:4001");
    println!();
    println!("Try:");
    println!(
        r#"  curl -X POST http://localhost:4001/process -H "Content-Type: application/json" -d '{{"text": "Hello World From Cano"}}'"#
    );

    axum::serve(listener, app).await.expect("server error");
}
