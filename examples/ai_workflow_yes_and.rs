//! # AI "Yes, and..." Improv Workflow Example
//!
//! This example demonstrates an improvisational storytelling conversation between two AI nodes:
//! 1. **Actor1Node**: Starts a story based on a random subject
//! 2. **Actor2Node**: Continues the story using "Yes, and..." improv principles
//!
//! The workflow uses rig-core with Ollama for text generation and facilitates a
//! collaborative storytelling session where each actor builds upon the previous contribution.
//!
//! The "Yes, and..." rule from improvisational comedy:
//! - Accept what the previous actor said (the "Yes")
//! - Add new information or details (the "and")
//! - Keep the story flowing and building
//!
//! Prerequisites:
//! - Install Ollama: https://ollama.ai/
//! - Run Ollama locally
//!
//! Run with:
//! ```bash
//! cargo run --example ai_workflow_yes_and
//! ```

use async_trait::async_trait;
use cano::prelude::*;
use rand::Rng;
use rig::client::CompletionClient;
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

const MODEL: &str = "qwen3:1.7b";
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

/// Utility function to filter out think tags from AI responses
///
/// Removes `<think>...</think>` blocks that AI models sometimes include
/// for internal reasoning that shouldn't appear in the final output.
fn filter_think_tags(text: &str) -> String {
    let mut result = text.to_string();

    // Remove <think>...</think> blocks (case insensitive)
    while let Some(start) = result.to_lowercase().find("<think>") {
        if let Some(end) = result[start..].to_lowercase().find("</think>") {
            let end_pos = start + end + "</think>".len();
            result.replace_range(start..end_pos, "");
        } else {
            // If no closing tag, remove from <think> to end
            result.replace_range(start.., "");
            break;
        }
    }

    // Clean up remaining tags and normalize whitespace
    result = result
        .replace("<think>", "")
        .replace("</think>", "")
        .replace("<THINK>", "")
        .replace("</THINK>", "");

    // Normalize whitespace
    result
        .lines()
        .map(|line| line.trim())
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
        .trim()
        .to_string()
}

/// Get conversation history from store as a formatted string
fn get_conversation_history(store: &MemoryStore) -> Result<String, CanoError> {
    let chat_history: Vec<String> = store
        .get::<Vec<String>>("chat")
        .unwrap_or_else(|_| Vec::new());
    Ok(chat_history.join("\n"))
}

/// Update and check interaction count
fn update_interaction_count(store: &MemoryStore) -> Result<u32, CanoError> {
    let current_count: u32 = store.get::<u32>("interaction_count").unwrap_or(0);
    let new_count = current_count + 1;
    store
        .put("interaction_count", new_count)
        .map_err(|e| CanoError::Store(format!("Failed to update interaction count: {e}")))?;
    Ok(new_count)
}

/// Create an AI agent with standard configuration
fn create_agent(client: &Client) -> rig::agent::Agent<rig::providers::ollama::CompletionModel> {
    client
        .agent(MODEL)
        .preamble(CONTEXT)
        .max_tokens(MAX_TOKEN)
        .build()
}

/// Conversation states for the "Yes, and..." improv workflow
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum ConversationState {
    Start,      // Actor1 starts the subject
    Actor2Turn, // Actor2's turn to continue with "Yes, and..."
    Actor1Turn, // Actor1's turn to continue with "Yes, and..."
    End,        // End the conversation after MAX_INTERACTIONS interactions
    Error,      // Error state
}

/// Actor1 node that starts stories and continues with "Yes, and..."
struct Actor1Node {
    client: Client,
}

impl Clone for Actor1Node {
    fn clone(&self) -> Self {
        Self {
            client: self.client.clone(),
        }
    }
}

impl Actor1Node {
    async fn new() -> Result<Self, CanoError> {
        Ok(Self {
            client: Client::new(),
        })
    }

    fn get_random_subject(&self) -> &str {
        let mut rng = rand::rng();
        let n = rng.random_range(0..SUBJECTS.len());
        SUBJECTS[n]
    }
}

#[async_trait]
impl Node<ConversationState> for Actor1Node {
    type PrepResult = String;
    type ExecResult = String;

    async fn prep(&self, store: &MemoryStore) -> Result<Self::PrepResult, CanoError> {
        get_conversation_history(store)
    }

    async fn exec(&self, prep_result: Self::PrepResult) -> Self::ExecResult {
        let subject = self.get_random_subject();
        let is_empty = prep_result.is_empty();

        let prompt = if is_empty {
            format!(
                "Start a creative short story about {subject}. Write 1 short sentence to set up an interesting scenario. Make it engaging and leave room for others to build upon it."
            )
        } else {
            prep_result
        };

        let agent = create_agent(&self.client);

        match agent.prompt(&prompt).await {
            Ok(response) => filter_think_tags(&response),
            Err(e) => {
                eprintln!("Actor1Node AI error: {e:?}");
                if is_empty {
                    format!("Say a story about the {subject}.")
                } else {
                    "Yes, and suddenly everything changed in ways no one could have predicted."
                        .to_string()
                }
            }
        }
    }

    async fn post(
        &self,
        store: &MemoryStore,
        exec_result: Self::ExecResult,
    ) -> Result<ConversationState, CanoError> {
        store
            .append("chat", exec_result.clone())
            .map_err(|e| CanoError::Store(format!("Failed to append to chat: {e}")))?;

        println!("🎭 Actor1: {exec_result}\n");

        let interaction_count = update_interaction_count(store)?;

        if interaction_count >= MAX_INTERACTIONS {
            Ok(ConversationState::End)
        } else {
            Ok(ConversationState::Actor2Turn)
        }
    }
}

/// Actor2 node that continues stories with "Yes, and..."
struct Actor2Node {
    client: Client,
}

impl Actor2Node {
    async fn new() -> Result<Self, CanoError> {
        Ok(Self {
            client: Client::new(),
        })
    }

    fn ensure_yes_and_format(&self, response: &str) -> String {
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

#[async_trait]
impl Node<ConversationState> for Actor2Node {
    type PrepResult = String;
    type ExecResult = String;

    async fn prep(&self, store: &MemoryStore) -> Result<Self::PrepResult, CanoError> {
        get_conversation_history(store)
    }

    async fn exec(&self, prep_result: Self::PrepResult) -> Self::ExecResult {
        let agent = create_agent(&self.client);

        match agent.prompt(prep_result).await {
            Ok(response) => self.ensure_yes_and_format(&response),
            Err(e) => {
                eprintln!("Actor2Node AI error: {e:?}");
                "Yes, and something unexpected happened that changed everything.".to_string()
            }
        }
    }

    async fn post(
        &self,
        store: &MemoryStore,
        exec_result: Self::ExecResult,
    ) -> Result<ConversationState, CanoError> {
        store
            .append("chat", exec_result.clone())
            .map_err(|e| CanoError::Store(format!("Failed to append to chat: {e}")))?;

        update_interaction_count(store)?;

        println!("🎪 Actor2: {exec_result}\n");

        Ok(ConversationState::Actor1Turn)
    }
}

#[tokio::main]
async fn main() -> Result<(), CanoError> {
    println!("🎭 Starting 'Yes, and...' Improv Workflow");
    println!("==========================================");
    println!("Using rig-core with Ollama and {MODEL}");
    println!("Make sure Ollama is running and you have pulled the {MODEL} model:");
    println!("  ollama pull {MODEL}");
    println!();
    println!("🎪 Rules of 'Yes, and...' Improv:");
    println!("   • Accept what your partner says (the 'Yes')");
    println!("   • Add new information to build the story (the 'and')");
    println!("   • Keep the story moving forward creatively");
    println!("   • {MAX_INTERACTIONS} total interactions to create a complete story");
    println!();

    // Create actors
    let actor1 = Actor1Node::new().await?;
    let actor2 = Actor2Node::new().await?;

    // Setup workflow
    let mut workflow = Workflow::new(ConversationState::Start);
    workflow
        .register_node(ConversationState::Start, actor1.clone())
        .register_node(ConversationState::Actor1Turn, actor1)
        .register_node(ConversationState::Actor2Turn, actor2)
        .add_exit_states(vec![ConversationState::End, ConversationState::Error]);

    let store = MemoryStore::new();

    println!("🚀 Starting improvised story...\n");

    let final_state = workflow.orchestrate(&store).await?;

    println!("\n🎯 Story completed with state: {final_state:?}");

    Ok(())
}
