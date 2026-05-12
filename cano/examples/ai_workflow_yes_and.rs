//! # AI "Yes, and..." Improv Workflow Example
//!
//! Two AI tasks carry a story forward using the "Yes, and..." improv rule:
//! - **Actor1Task** opens with a random subject; later turns continue the thread.
//! - **Actor2Task** continues every other turn with "Yes, and...".
//!
//! The example wraps the rig-core Ollama client in an `OllamaResource` so the
//! HTTP client is built once at workflow setup, shared across both actors via
//! `Resources`, and torn down in one place. Actors hold no client state.
//!
//! Prerequisites:
//! - Install Ollama: <https://ollama.ai/>
//! - `ollama pull gemma4:e2b` and run the daemon
//!
//! Run with:
//! ```bash
//! cargo run --example ai_workflow_yes_and
//! ```

use cano::prelude::*;
use rand::RngExt;
use rig::client::{CompletionClient, Nothing};
use rig::completion::Prompt;
use rig::providers::ollama::Client;

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

const MODEL: &str = "gemma4:e2b";
const MAX_TOKEN: u64 = 2048;
const MAX_INTERACTIONS: u32 = 12;

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
// OllamaResource — shared rig-core client
// ============================================================================

/// Wraps the rig Ollama `Client` so it lives as a workflow resource.
///
/// `setup` is a no-op print today (the rig client is constructed eagerly in
/// `new`), but real deployments could ping the model endpoint here to fail
/// fast if Ollama is unreachable.
struct OllamaResource {
    client: Client,
}

impl OllamaResource {
    fn new() -> Result<Self, CanoError> {
        Ok(Self {
            client: Client::new(Nothing)
                .map_err(|e| CanoError::Generic(format!("Ollama client error: {e}")))?,
        })
    }

    /// Build a fresh agent for one prompt round.
    fn agent(&self) -> rig::agent::Agent<rig::providers::ollama::CompletionModel<reqwest::Client>> {
        self.client
            .agent(MODEL)
            .preamble(CONTEXT)
            .max_tokens(MAX_TOKEN)
            .build()
    }
}

#[resource]
impl Resource for OllamaResource {
    async fn setup(&self) -> Result<(), CanoError> {
        println!("OllamaResource: ready (model={MODEL})");
        Ok(())
    }
}

// ============================================================================
// Helpers
// ============================================================================

/// Removes `<think>...</think>` reasoning blocks and normalizes whitespace.
fn filter_think_tags(text: &str) -> String {
    let mut result = text.to_string();

    while let Some(start) = result.to_lowercase().find("<think>") {
        if let Some(end) = result[start..].to_lowercase().find("</think>") {
            let end_pos = start + end + "</think>".len();
            result.replace_range(start..end_pos, "");
        } else {
            result.replace_range(start.., "");
            break;
        }
    }

    result = result
        .replace("<think>", "")
        .replace("</think>", "")
        .replace("<THINK>", "")
        .replace("</THINK>", "");

    result
        .lines()
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
        let ollama = res.get::<OllamaResource, _>("ollama")?;
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

        let response = match ollama.agent().prompt(&prompt).await {
            Ok(r) => filter_think_tags(&r),
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
        let cleaned = filter_think_tags(response);
        if cleaned.trim().to_lowercase().starts_with("yes, and") {
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
        let ollama = res.get::<OllamaResource, _>("ollama")?;
        let history = get_conversation_history(&store)?;

        let response = match ollama.agent().prompt(&history).await {
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
    println!("Starting 'Yes, and...' Improv Workflow");
    println!("==========================================");
    println!("Using rig-core with Ollama and {MODEL}");
    println!("Make sure Ollama is running and you have pulled the {MODEL} model:");
    println!("  ollama pull {MODEL}");
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
        .insert("ollama", OllamaResource::new()?);

    let workflow = Workflow::new(resources)
        .register(ConversationState::Start, Actor1Task)
        .register(ConversationState::Actor1Turn, Actor1Task)
        .register(ConversationState::Actor2Turn, Actor2Task)
        .add_exit_states(vec![ConversationState::End, ConversationState::Error]);

    println!("Starting improvised story...\n");

    let final_state = workflow.orchestrate(ConversationState::Start).await?;

    println!("\nStory completed with state: {final_state:?}");

    Ok(())
}
