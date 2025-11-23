#![cfg(feature = "scheduler")]
//! # Book Preposition Analysis Example
//!
//! This example demonstrates a book analysis system that:
//! 1. Downloads popular books from Project Gutenberg
//! 2. Analyzes preposition usage in each book
//! 3. Ranks books by preposition diversity
//!
//! For concurrent downloads, see the workflow_concurrent.rs example which demonstrates
//! the split/join pattern for parallel task execution.
//!
//! ## Execution
//!
//! ```bash
//! cargo run --example scheduler_book_prepositions --features scheduler
//! ```

use async_trait::async_trait;
use cano::error::CanoError;
use cano::prelude::*;
use std::collections::HashSet;
use std::time::Duration;
use tokio::time::timeout;

/// Workflow state management
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum WorkflowPhase {
    Download,
    Analyze,
    Rank,
    Complete,
}

/// Book data structure
#[derive(Debug, Clone)]
struct Book {
    id: u32,
    title: String,
    content: String,
}

/// Book analysis result
#[derive(Debug, Clone)]
struct BookAnalysis {
    book_id: u32,
    title: String,
    preposition_count: usize,
    total_words: usize,
    preposition_density: f64,
}

/// Book ranking result
#[derive(Debug, Clone)]
struct BookRanking {
    rank: usize,
    analysis: BookAnalysis,
}

/// Common English prepositions for analysis
const PREPOSITIONS: &[&str] = &[
    "about", "above", "across", "after", "against", "along", "among", "around", "at", "before",
    "behind", "below", "beneath", "beside", "between", "beyond", "by", "down", "during", "for",
    "from", "in", "inside", "into", "near", "of", "off", "on", "onto", "over", "through", "to",
    "under", "up", "with", "within",
];

/// Download Node: Downloads a book from Project Gutenberg
#[derive(Clone)]
struct BookDownloadNode {
    book_id: u32,
    title: String,
    url: String,
}

impl BookDownloadNode {
    fn new(book_id: u32, title: String, url: String) -> Self {
        Self {
            book_id,
            title,
            url,
        }
    }

    async fn download_book(&self) -> Result<Book, String> {
        println!("📚 Downloading: {}", self.title);

        let client = reqwest::Client::new();

        let download_future = async {
            let response = client
                .get(&self.url)
                .send()
                .await
                .map_err(|e| format!("Failed to fetch {}: {}", self.url, e))?;

            if !response.status().is_success() {
                return Err(format!(
                    "HTTP error for {}: {}",
                    self.title,
                    response.status()
                ));
            }

            let content = response
                .text()
                .await
                .map_err(|e| format!("Failed to read content for {}: {}", self.title, e))?;

            if content.len() < 1000 {
                return Err(format!(
                    "Content too short for {}, might be an error page",
                    self.title
                ));
            }

            println!("✅ Downloaded: {} ({} chars)", self.title, content.len());

            Ok(Book {
                id: self.book_id,
                title: self.title.clone(),
                content,
            })
        };

        match timeout(Duration::from_secs(30), download_future).await {
            Ok(result) => result,
            Err(_) => Err(format!("Timeout downloading {}", self.title)),
        }
    }
}

#[async_trait]
impl Node<WorkflowPhase> for BookDownloadNode {
    type PrepResult = ();
    type ExecResult = Option<Book>;

    fn config(&self) -> TaskConfig {
        TaskConfig::new().with_fixed_retry(2, Duration::from_secs(1))
    }

    async fn prep(&self, _store: &MemoryStore) -> Result<Self::PrepResult, CanoError> {
        Ok(())
    }

    async fn exec(&self, _prep_res: Self::PrepResult) -> Self::ExecResult {
        match self.download_book().await {
            Ok(book) => Some(book),
            Err(error) => {
                eprintln!("❌ Download failed for {}: {}", self.title, error);
                None
            }
        }
    }

    async fn post(
        &self,
        store: &MemoryStore,
        exec_res: Self::ExecResult,
    ) -> Result<WorkflowPhase, CanoError> {
        if let Some(book) = exec_res {
            // Add book to collection
            let mut books: Vec<Book> = store.get("books").unwrap_or_else(|_| Vec::new());
            books.push(book);
            store.put("books", books)?;
            Ok(WorkflowPhase::Analyze)
        } else {
            Ok(WorkflowPhase::Complete)
        }
    }
}

/// Analysis Node: Analyzes prepositions in downloaded books
#[derive(Clone)]
struct PrepositionAnalysisNode;

impl PrepositionAnalysisNode {
    fn analyze_book_prepositions(book: &Book) -> BookAnalysis {
        let preposition_set: HashSet<&str> = PREPOSITIONS.iter().copied().collect();
        let mut found_prepositions = HashSet::new();

        let content_lower = book.content.to_lowercase();
        let words: Vec<&str> = content_lower
            .split_whitespace()
            .map(|word| word.trim_matches(|c: char| !c.is_alphabetic()))
            .filter(|word| !word.is_empty())
            .collect();

        let total_words = words.len();

        for word in words {
            if preposition_set.contains(word) {
                found_prepositions.insert(word.to_string());
            }
        }

        let preposition_density = if total_words > 0 {
            (found_prepositions.len() as f64 / total_words as f64) * 100.0
        } else {
            0.0
        };

        println!(
            "📖 Analyzed '{}': {} unique prepositions, {:.2}% density",
            book.title,
            found_prepositions.len(),
            preposition_density
        );

        BookAnalysis {
            book_id: book.id,
            title: book.title.clone(),
            preposition_count: found_prepositions.len(),
            total_words,
            preposition_density,
        }
    }
}

#[async_trait]
impl Node<WorkflowPhase> for PrepositionAnalysisNode {
    type PrepResult = Vec<Book>;
    type ExecResult = Vec<BookAnalysis>;

    async fn prep(&self, store: &MemoryStore) -> Result<Self::PrepResult, CanoError> {
        let books: Vec<Book> = store
            .get("books")
            .map_err(|e| CanoError::preparation(format!("Failed to load books: {}", e)))?;
        println!("📚 Loaded {} books for preposition analysis", books.len());
        Ok(books)
    }

    async fn exec(&self, prep_res: Self::PrepResult) -> Self::ExecResult {
        println!("🔍 Analyzing prepositions in {} books...", prep_res.len());

        prep_res
            .iter()
            .map(Self::analyze_book_prepositions)
            .collect()
    }

    async fn post(
        &self,
        store: &MemoryStore,
        exec_res: Self::ExecResult,
    ) -> Result<WorkflowPhase, CanoError> {
        if exec_res.is_empty() {
            return Err(CanoError::node_execution("No book analyses were completed"));
        }

        let count = exec_res.len();
        store.put("book_analyses", exec_res)?;
        println!("✅ Stored {} book analyses", count);
        Ok(WorkflowPhase::Rank)
    }
}

/// Ranking Node: Ranks books by preposition diversity
#[derive(Clone)]
struct BookRankingNode;

#[async_trait]
impl Node<WorkflowPhase> for BookRankingNode {
    type PrepResult = Vec<BookAnalysis>;
    type ExecResult = Vec<BookRanking>;

    async fn prep(&self, store: &MemoryStore) -> Result<Self::PrepResult, CanoError> {
        let analyses: Vec<BookAnalysis> = store
            .get("book_analyses")
            .map_err(|e| CanoError::preparation(format!("Failed to load analyses: {}", e)))?;
        println!("📊 Loaded {} book analyses for ranking", analyses.len());
        Ok(analyses)
    }

    async fn exec(&self, prep_res: Self::PrepResult) -> Self::ExecResult {
        println!("🏆 Ranking books by preposition diversity...");

        let mut analyses = prep_res;
        analyses.sort_by(|a, b| b.preposition_count.cmp(&a.preposition_count));

        analyses
            .into_iter()
            .enumerate()
            .map(|(index, analysis)| BookRanking {
                rank: index + 1,
                analysis,
            })
            .collect()
    }

    async fn post(
        &self,
        store: &MemoryStore,
        exec_res: Self::ExecResult,
    ) -> Result<WorkflowPhase, CanoError> {
        store.put("book_rankings", exec_res.clone())?;

        println!("\n🏆 BOOK RANKINGS BY PREPOSITION DIVERSITY");
        println!("==========================================");

        for ranking in &exec_res {
            println!(
                "#{}: {} (ID: {})",
                ranking.rank, ranking.analysis.title, ranking.analysis.book_id
            );
            println!(
                "    📊 {} unique prepositions | {:.2}% density | {} total words",
                ranking.analysis.preposition_count,
                ranking.analysis.preposition_density,
                ranking.analysis.total_words
            );
        }

        Ok(WorkflowPhase::Complete)
    }
}

#[tokio::main]
async fn main() -> CanoResult<()> {
    println!("📚 Book Preposition Analysis Example");
    println!("=====================================\n");

    let store = MemoryStore::new();

    // Download a few sample books
    println!("📥 Downloading books from Project Gutenberg...\n");

    // Book 1: Pride and Prejudice
    let workflow1 = Workflow::new(store.clone())
        .register(
            WorkflowPhase::Download,
            BookDownloadNode::new(
                1342,
                "Pride and Prejudice by Jane Austen".to_string(),
                "https://www.gutenberg.org/files/1342/1342-0.txt".to_string(),
            ),
        )
        .add_exit_states(vec![WorkflowPhase::Analyze, WorkflowPhase::Complete]);

    let _ = workflow1.orchestrate(WorkflowPhase::Download).await?;

    // Book 2: Alice's Adventures in Wonderland
    let workflow2 = Workflow::new(store.clone())
        .register(
            WorkflowPhase::Download,
            BookDownloadNode::new(
                11,
                "Alice's Adventures in Wonderland".to_string(),
                "https://www.gutenberg.org/files/11/11-0.txt".to_string(),
            ),
        )
        .add_exit_states(vec![WorkflowPhase::Analyze, WorkflowPhase::Complete]);

    let _ = workflow2.orchestrate(WorkflowPhase::Download).await?;

    // Book 3: A Christmas Carol
    let workflow3 = Workflow::new(store.clone())
        .register(
            WorkflowPhase::Download,
            BookDownloadNode::new(
                46,
                "A Christmas Carol by Charles Dickens".to_string(),
                "https://www.gutenberg.org/files/46/46-0.txt".to_string(),
            ),
        )
        .add_exit_states(vec![WorkflowPhase::Analyze, WorkflowPhase::Complete]);

    let _ = workflow3.orchestrate(WorkflowPhase::Download).await?;

    // Analyze and rank the downloaded books
    println!("\n📊 Analyzing and ranking books...\n");

    let analysis_workflow = Workflow::new(store.clone())
        .register(WorkflowPhase::Analyze, PrepositionAnalysisNode)
        .register(WorkflowPhase::Rank, BookRankingNode)
        .add_exit_state(WorkflowPhase::Complete);

    analysis_workflow
        .orchestrate(WorkflowPhase::Analyze)
        .await?;

    println!("\n✅ Book preposition analysis complete!");
    println!("\n💡 Note: For concurrent downloads using split/join patterns,");
    println!("   see examples/workflow_concurrent.rs");

    Ok(())
}
