//! # AI "Yes, and..." Improv Workflow Example
//!
//! Two AI tasks carry a story forward using the "Yes, and..." improv rule:
//! - **Actor1Task** opens with a random subject; later turns continue the thread.
//! - **Actor2Task** continues every other turn with "Yes, and...".
//!
//! The example wraps a [`cuca`](https://crates.io/crates/cuca) `CucaClient` in a
//! `CucaResource` so the HTTP client is built once at workflow setup, shared
//! across both actors via `Resources`, and torn down in one place. Actors hold
//! no client state.
//!
//! Prerequisites:
//! - An OpenAI-compatible local inference server listening on port 1234 —
//!   [LM Studio](https://lmstudio.ai/) is the default target, but llama.cpp,
//!   vLLM, or anything else speaking `/v1/chat/completions` works.
//! - The `google/gemma-4-12b-qat` model loaded and served.
//! - A context window that holds the transcript plus `MAX_TOKEN` completion
//!   tokens. The demo model's 8192-token window (LM Studio's default) is
//!   plenty for both.
//!
//! Configuration (both optional, defaults target a local LM Studio server):
//! - `CUCA_BASE_URL`: server base URL, defaults to `http://127.0.0.1:1234/v1`.
//! - `CUCA_MODEL`: upstream model id, defaults to `google/gemma-4-12b-qat`.
//!
//! Run with:
//! ```bash
//! cargo run --example ai_workflow_yes_and
//! ```
//!
//! Or point it at a server on the network:
//! ```bash
//! CUCA_BASE_URL=http://192.168.1.10:1234/v1 CUCA_MODEL=google/gemma-4-12b-qat \
//!   cargo run --example ai_workflow_yes_and
//! ```

use cano::prelude::*;
use cuca::types::{MessageContentBlock, ProviderEndpoint};
use cuca::{CucaClient, ThinkingConfig, ThinkingParams, UnifiedRequest};
use futures_util::StreamExt;
use rand::RngExt;

// Configuration constants
const CONTEXT: &str = r#"
Continue talking about the subject using the 'Yes, and...' improv principle:
- Accept what was said before WITHOUT REPEATING IT
- Add something new to the conversation
- Start your response with 'Yes, and...'
- Keep responses brief: minimum 10, maximum 20 words
- Avoid repeating previous parts of the conversation
- Feel free to use any object from the previous conversation
"#;

const DEFAULT_BASE_URL: &str = "http://127.0.0.1:1234/v1";
const DEFAULT_MODEL: &str = "google/gemma-4-12b-qat";
/// Completion-token budget for one reply.
///
/// The visible replies are one-liners of 10-20 words, so this is already
/// generous. The ceiling is the server's context window (8192 for the demo
/// model), which has to hold the transcript as well as this budget.
const MAX_TOKEN: u32 = 2048;
const MAX_INTERACTIONS: u32 = 12;

/// Server base URL: `CUCA_BASE_URL`, falling back to a local LM Studio server.
fn base_url() -> String {
    std::env::var("CUCA_BASE_URL").unwrap_or_else(|_| DEFAULT_BASE_URL.to_string())
}

/// Upstream model id: `CUCA_MODEL`, falling back to the demo model.
fn model_id() -> String {
    std::env::var("CUCA_MODEL").unwrap_or_else(|_| DEFAULT_MODEL.to_string())
}

// Story subjects for random selection
const SUBJECTS: &[&str] = &[
    "cats",
    "programming",
    "coffee",
    "weather",
    "cooking",
    "books",
    "movies",
    "music",
    "travel",
    "technology",
    "art",
    "history",
    "science",
    "sports",
    "gaming",
    "food",
    "nature",
    "health",
    "fashion",
    "photography",
    "education",
    "relationships",
    "philosophy",
    "psychology",
    "economics",
];

// ============================================================================
// CucaResource — shared cuca client
// ============================================================================

/// Wraps a single `CucaClient` so it lives as a workflow resource.
///
/// `setup` is a no-op print today (the client is constructed eagerly in `new`),
/// but real deployments could ping the model endpoint here to fail fast if the
/// inference server is unreachable.
struct CucaResource {
    client: CucaClient,
    model: String,
}

impl CucaResource {
    fn new() -> Result<Self, CanoError> {
        Ok(Self {
            client: CucaClient::builder()
                .with_provider(ProviderEndpoint::LmStudio)
                .with_base_url(base_url())
                .build()
                .map_err(|e| CanoError::Generic(format!("Cuca client error: {e}")))?,
            model: model_id(),
        })
    }

    /// Run one completion round against the shared client.
    ///
    /// `generate_stream` is cuca's single generation entry point, so the
    /// streamed blocks are drained here and the `Text` payloads concatenated
    /// into one reply. `Thinking` blocks carry the model's reasoning and are
    /// dropped — no `<think>` tag scraping needed.
    ///
    /// The explicit `reasoning_effort: "none"` is deliberate — **do not remove
    /// it**. `google/gemma-4-12b-qat` reasons by default, and cuca emits
    /// `reasoning_effort` only when `thinking` is set, so leaving `thinking`
    /// unset — or setting `ThinkingConfig::disabled()`, which also omits the
    /// key — lets the server apply its own default and spend the whole
    /// completion budget on reasoning, ending on `finish_reason: "length"`
    /// with empty content. This server honours only `"none"` as off
    /// (`Minimal` and `Low` still reason to exhaustion), so the raw
    /// `ThinkingParams::OpenAi` override is what buys a plain completion.
    async fn complete(&self, prompt: &str) -> Result<String, CanoError> {
        let request = UnifiedRequest::new(self.model.clone())
            .add_system_message(CONTEXT)
            .add_user_message(prompt)
            .set_max_tokens(MAX_TOKEN)
            .with_thinking(ThinkingConfig {
                enabled: true,
                effort: None,
                params: ThinkingParams::OpenAi {
                    reasoning_effort: Some("none".to_string()),
                },
            });

        let mut stream = self
            .client
            .generate_stream(request)
            .await
            .map_err(|e| CanoError::Generic(format!("Cuca completion error: {e}")))?;

        let mut answer = String::new();
        while let Some(block) = stream.next().await {
            let block = block.map_err(|e| CanoError::Generic(format!("Cuca stream error: {e}")))?;
            if let MessageContentBlock::Text(text) = block {
                answer.push_str(&text);
            }
        }

        if answer.trim().is_empty() {
            return Err(CanoError::Generic(
                "model returned no text blocks: the stream ended without a visible reply"
                    .to_string(),
            ));
        }

        Ok(answer)
    }
}

#[resource]
impl Resource for CucaResource {
    async fn setup(&self) -> Result<(), CanoError> {
        println!(
            "CucaResource: ready (model={}, url={})",
            self.model,
            base_url()
        );
        Ok(())
    }
}

// ============================================================================
// Helpers
// ============================================================================

/// Collapse a streamed reply into a single trimmed line.
fn normalize(text: &str) -> String {
    text.lines()
        .map(|line| line.trim())
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
        .trim()
        .to_string()
}

/// Read the rolling chat transcript from the in-memory store.
fn get_conversation_history(store: &MemoryStore) -> Result<String, CanoError> {
    let chat_history: Vec<String> = store
        .get::<Vec<String>>("chat")
        .unwrap_or_else(|_| Vec::new());
    Ok(chat_history.join("\n"))
}

/// Increment the per-workflow interaction counter and return the new value.
fn update_interaction_count(store: &MemoryStore) -> Result<u32, CanoError> {
    let current_count: u32 = store.get::<u32>("interaction_count").unwrap_or(0);
    let new_count = current_count + 1;
    store
        .put("interaction_count", new_count)
        .map_err(|e| CanoError::Store(format!("Failed to update interaction count: {e}")))?;
    Ok(new_count)
}

// ============================================================================
// Workflow states
// ============================================================================

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum ConversationState {
    Start,      // Actor1 opens with a random subject
    Actor2Turn, // Actor2's turn to extend with "Yes, and..."
    Actor1Turn, // Actor1's turn to extend with "Yes, and..."
    End,        // Reached MAX_INTERACTIONS
    Error,      // Reserved for future error transitions
}

// ============================================================================
// Actor1Task
// ============================================================================

/// Opens the story with a random subject; on later turns extends the thread.
#[derive(Clone)]
struct Actor1Task;

impl Actor1Task {
    fn pick_subject() -> &'static str {
        let mut rng = rand::rng();
        SUBJECTS[rng.random_range(0..SUBJECTS.len())]
    }
}

#[task(state = ConversationState)]
impl Actor1Task {
    async fn run(&self, res: &Resources) -> Result<TaskResult<ConversationState>, CanoError> {
        let store = res.get::<MemoryStore, _>("store")?;
        let cuca = res.get::<CucaResource, _>("cuca")?;
        let history = get_conversation_history(&store)?;

        let subject = Self::pick_subject();
        let is_empty = history.is_empty();

        let prompt = if is_empty {
            format!(
                "Start a creative short story about {subject}. Write 1 short sentence to set up an interesting scenario. Make it engaging and leave room for others to build upon it."
            )
        } else {
            history
        };

        let response = match cuca.complete(&prompt).await {
            Ok(r) => normalize(&r),
            Err(e) => {
                eprintln!("Actor1Task AI error: {e:?}");
                if is_empty {
                    format!("Say a story about the {subject}.")
                } else {
                    "Yes, and suddenly everything changed in ways no one could have predicted."
                        .to_string()
                }
            }
        };

        store
            .append("chat", response.clone())
            .map_err(|e| CanoError::Store(format!("Failed to append to chat: {e}")))?;

        println!("Actor1: {response}\n");

        let interaction_count = update_interaction_count(&store)?;

        let next = if interaction_count >= MAX_INTERACTIONS {
            ConversationState::End
        } else {
            ConversationState::Actor2Turn
        };

        Ok(TaskResult::Single(next))
    }
}

// ============================================================================
// Actor2Task
// ============================================================================

/// Always answers with "Yes, and..." — the improv rule guard rail.
#[derive(Clone)]
struct Actor2Task;

impl Actor2Task {
    fn ensure_yes_and_format(response: &str) -> String {
        let cleaned = normalize(response);
        if cleaned.to_lowercase().starts_with("yes, and") {
            cleaned
        } else {
            format!(
                "Yes, and {}",
                cleaned
                    .trim_start_matches("And ")
                    .trim_start_matches("and ")
            )
        }
    }
}

#[task(state = ConversationState)]
impl Actor2Task {
    async fn run(&self, res: &Resources) -> Result<TaskResult<ConversationState>, CanoError> {
        let store = res.get::<MemoryStore, _>("store")?;
        let cuca = res.get::<CucaResource, _>("cuca")?;
        let history = get_conversation_history(&store)?;

        let response = match cuca.complete(&history).await {
            Ok(r) => Self::ensure_yes_and_format(&r),
            Err(e) => {
                eprintln!("Actor2Task AI error: {e:?}");
                "Yes, and something unexpected happened that changed everything.".to_string()
            }
        };

        store
            .append("chat", response.clone())
            .map_err(|e| CanoError::Store(format!("Failed to append to chat: {e}")))?;

        update_interaction_count(&store)?;

        println!("Actor2: {response}\n");

        Ok(TaskResult::Single(ConversationState::Actor1Turn))
    }
}

// ============================================================================
// Main
// ============================================================================

#[tokio::main]
async fn main() -> Result<(), CanoError> {
    let model = model_id();
    let url = base_url();

    println!("Starting 'Yes, and...' Improv Workflow");
    println!("==========================================");
    println!("Using cuca against an OpenAI-compatible server");
    println!("  endpoint: {url}");
    println!("  model:    {model}");
    println!("Load {model} in LM Studio (or any server on port 1234) before running.");
    println!("Override with the CUCA_BASE_URL and CUCA_MODEL environment variables.");
    println!();
    println!("Rules of 'Yes, and...' Improv:");
    println!("   Accept what your partner says (the 'Yes')");
    println!("   Add new information to build the story (the 'and')");
    println!("   Keep the story moving forward creatively");
    println!("   {MAX_INTERACTIONS} total interactions to create a complete story");
    println!();

    let store = MemoryStore::new();
    let resources = Resources::new()
        .insert("store", store.clone())
        .insert("cuca", CucaResource::new()?);

    let workflow = Workflow::new(resources)
        .register(ConversationState::Start, Actor1Task)
        .register(ConversationState::Actor1Turn, Actor1Task)
        .register(ConversationState::Actor2Turn, Actor2Task)
        .add_exit_states(vec![ConversationState::End, ConversationState::Error]);

    println!("Starting improvised story...\n");

    let final_state = workflow
        .orchestrate(ConversationState::Start, CancellationToken::disabled())
        .await?;

    println!("\nStory completed with state: {final_state:?}");

    Ok(())
}
