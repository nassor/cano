#![cfg(feature = "scheduler")]
//! # Multi-Level Map-Reduce Book Analysis Example
//!
//! This example demonstrates a sophisticated two-level map-reduce pattern:
//!
//! **Level 1 (Workflow Level)**: Each workflow processes a batch of books using Split/Join
//! - Split: Download and analyze multiple books in parallel within one workflow
//! - Join: Aggregate results from that batch
//!
//! **Level 2 (Scheduler Level)**: Multiple workflows process different batches
//! - Map: Each workflow handles a different set of books (different parameters)
//! - Reduce: After all workflows complete, aggregate results across all batches
//!
//! ## Architecture
//!
//! ```text
//! Scheduler (Level 2 Map-Reduce)
//! ├── Workflow 1: Batch A (books 1-4)  ─┐
//! │   ├── Split: Download 4 books      │
//! │   ├── Split: Analyze 4 books       │
//! │   └── Join: Batch summary          │
//! ├── Workflow 2: Batch B (books 5-8)  ├─→ Final Reduce
//! │   ├── Split: Download 4 books      │   (Global Rankings)
//! │   ├── Split: Analyze 4 books       │
//! │   └── Join: Batch summary          │
//! └── Workflow 3: Batch C (books 9-12) ─┘
//!     ├── Split: Download 4 books
//!     ├── Split: Analyze 4 books
//!     └── Join: Batch summary
//! ```
//!
//! ## Execution
//!
//! ```bash
//! cargo run --example scheduler_mapreduce_books --features scheduler
//! ```

use async_trait::async_trait;
use cano::error::CanoError;
use cano::prelude::*;
use cano::scheduler::Status;
use cano::store::{KeyValueStore, MemoryStore};
use std::collections::HashSet;
use std::sync::Arc;
use tokio::sync::RwLock;
use tokio::time::{Duration, timeout};

/// Workflow states
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum BookAnalysisState {
    Start,
    DownloadBatch,
    AnalyzeBatch,
    SummarizeBatch,
    Complete,
    Error,
}

/// Book data structure
#[derive(Debug, Clone)]
struct Book {
    id: u32,
    title: String,
    content: String,
    batch_name: String,
}

/// Book analysis result
#[derive(Debug, Clone)]
struct BookAnalysis {
    book_id: u32,
    title: String,
    batch_name: String,
    preposition_count: usize,
    total_words: usize,
    unique_prepositions: HashSet<String>,
}

/// Batch summary
#[derive(Debug, Clone)]
struct BatchSummary {
    batch_name: String,
    total_books: usize,
    avg_prepositions: f64,
    total_unique_prepositions: usize,
    book_analyses: Vec<BookAnalysis>,
}

/// Global shared state for collecting results from all workflows
#[derive(Debug, Clone)]
struct GlobalResults {
    batch_summaries: Arc<RwLock<Vec<BatchSummary>>>,
}

impl GlobalResults {
    fn new() -> Self {
        Self {
            batch_summaries: Arc::new(RwLock::new(Vec::new())),
        }
    }

    async fn add_batch(&self, summary: BatchSummary) {
        let mut summaries = self.batch_summaries.write().await;
        summaries.push(summary);
    }

    async fn get_all_batches(&self) -> Vec<BatchSummary> {
        let summaries = self.batch_summaries.read().await;
        summaries.clone()
    }
}

/// Common English prepositions
const PREPOSITIONS: &[&str] = &[
    "about", "above", "across", "after", "against", "along", "among", "around",
    "at", "before", "behind", "below", "beneath", "beside", "between", "beyond",
    "by", "down", "during", "for", "from", "in", "into", "near", "of", "off",
    "on", "over", "through", "to", "toward", "under", "up", "with", "within",
];

/// Book metadata (id, title, url)
type BookMetadata = (u32, String, String);

/// Get all books divided into batches
fn get_book_batches() -> Vec<(String, Vec<BookMetadata>)> {
    vec![
        (
            "Batch-A-Classics".to_string(),
            vec![
                (
                    1342,
                    "Pride and Prejudice by Jane Austen".to_string(),
                    "https://www.gutenberg.org/files/1342/1342-0.txt".to_string(),
                ),
                (
                    11,
                    "Alice's Adventures in Wonderland by Lewis Carroll".to_string(),
                    "https://www.gutenberg.org/files/11/11-0.txt".to_string(),
                ),
                (
                    84,
                    "Frankenstein by Mary Wollstonecraft Shelley".to_string(),
                    "https://www.gutenberg.org/files/84/84-0.txt".to_string(),
                ),
                (
                    1661,
                    "The Adventures of Sherlock Holmes by Arthur Conan Doyle".to_string(),
                    "https://www.gutenberg.org/files/1661/1661-0.txt".to_string(),
                ),
            ],
        ),
        (
            "Batch-B-Adventure".to_string(),
            vec![
                (
                    2701,
                    "Moby Dick by Herman Melville".to_string(),
                    "https://www.gutenberg.org/files/2701/2701-0.txt".to_string(),
                ),
                (
                    76,
                    "Adventures of Huckleberry Finn by Mark Twain".to_string(),
                    "https://www.gutenberg.org/files/76/76-0.txt".to_string(),
                ),
                (
                    345,
                    "Dracula by Bram Stoker".to_string(),
                    "https://www.gutenberg.org/files/345/345-0.txt".to_string(),
                ),
                (
                    174,
                    "The Picture of Dorian Gray by Oscar Wilde".to_string(),
                    "https://www.gutenberg.org/files/174/174-0.txt".to_string(),
                ),
            ],
        ),
        (
            "Batch-C-Romance".to_string(),
            vec![
                (
                    1260,
                    "Jane Eyre by Charlotte Brontë".to_string(),
                    "https://www.gutenberg.org/files/1260/1260-0.txt".to_string(),
                ),
                (
                    1513,
                    "Romeo and Juliet by William Shakespeare".to_string(),
                    "https://www.gutenberg.org/files/1513/1513-0.txt".to_string(),
                ),
                (
                    46,
                    "A Christmas Carol by Charles Dickens".to_string(),
                    "https://www.gutenberg.org/files/46/46-0.txt".to_string(),
                ),
                (
                    1080,
                    "A Modest Proposal by Jonathan Swift".to_string(),
                    "https://www.gutenberg.org/files/1080/1080-0.txt".to_string(),
                ),
            ],
        ),
    ]
}

/// Download a single book
async fn download_book(
    id: u32,
    title: String,
    url: String,
    batch_name: String,
) -> Result<Book, String> {
    println!("  📥 [{batch_name}] Downloading: {title}");

    let client = reqwest::Client::new();

    let download_future = async {
        let response = client
            .get(&url)
            .send()
            .await
            .map_err(|e| format!("Failed to fetch {url}: {e}"))?;

        if !response.status().is_success() {
            return Err(format!("HTTP error for {title}: {}", response.status()));
        }

        let content = response
            .text()
            .await
            .map_err(|e| format!("Failed to read content for {title}: {e}"))?;

        if content.len() < 1000 {
            return Err(format!("Content too short for {title}"));
        }

        println!(
            "  ✅ [{batch_name}] Downloaded: {title} ({} KB)",
            content.len() / 1024
        );

        Ok(Book {
            id,
            title: title.clone(),
            content,
            batch_name,
        })
    };

    timeout(Duration::from_secs(30), download_future)
        .await
        .map_err(|_| format!("Timeout downloading {title}"))?
}

/// Analyze prepositions in a book
fn analyze_prepositions(book: &Book) -> BookAnalysis {
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

    BookAnalysis {
        book_id: book.id,
        title: book.title.clone(),
        batch_name: book.batch_name.clone(),
        preposition_count: found_prepositions.len(),
        total_words,
        unique_prepositions: found_prepositions,
    }
}

/// Task: Initialize batch processing
#[derive(Clone)]
struct InitBatchTask {
    batch_name: String,
    books: Vec<BookMetadata>,
}

#[async_trait]
impl Task<BookAnalysisState> for InitBatchTask {
    async fn run(&self, store: &MemoryStore) -> Result<TaskResult<BookAnalysisState>, CanoError> {
        println!(
            "\n🎯 [{0}] Initializing batch with {1} books",
            self.batch_name,
            self.books.len()
        );

        store.put("batch_name", self.batch_name.clone())?;
        store.put("book_metadata", self.books.clone())?;

        Ok(TaskResult::Single(BookAnalysisState::DownloadBatch))
    }
}

/// Task: Download a single book (used in split)
#[derive(Clone)]
struct DownloadTask {
    book_id: u32,
    title: String,
    url: String,
    batch_name: String,
}

#[async_trait]
impl Task<BookAnalysisState> for DownloadTask {
    async fn run(&self, store: &MemoryStore) -> Result<TaskResult<BookAnalysisState>, CanoError> {
        match download_book(
            self.book_id,
            self.title.clone(),
            self.url.clone(),
            self.batch_name.clone(),
        )
        .await
        {
            Ok(book) => {
                // Store individual book
                store.put(&format!("book_{}", self.book_id), book)?;
                Ok(TaskResult::Single(BookAnalysisState::AnalyzeBatch))
            }
            Err(e) => Err(CanoError::task_execution(format!("Download failed: {e}"))),
        }
    }
}

/// Task: Split downloads across batch books
#[derive(Clone)]
struct SplitDownloadTask;

#[async_trait]
impl Task<BookAnalysisState> for SplitDownloadTask {
    async fn run(&self, store: &MemoryStore) -> Result<TaskResult<BookAnalysisState>, CanoError> {
        let batch_name: String = store.get("batch_name")?;
        let books: Vec<BookMetadata> = store.get("book_metadata")?;

        println!(
            "  📤 [{batch_name}] Splitting downloads for {} books",
            books.len()
        );

        // Return Split with all download states
        let states: Vec<BookAnalysisState> = books
            .iter()
            .map(|_| BookAnalysisState::AnalyzeBatch)
            .collect();

        // Store download tasks to be executed
        let download_tasks: Vec<_> = books
            .into_iter()
            .map(|(id, title, url)| DownloadTask {
                book_id: id,
                title,
                url,
                batch_name: batch_name.clone(),
            })
            .collect();

        store.put("download_tasks", download_tasks)?;

        Ok(TaskResult::Split(states))
    }
}

/// Task: Analyze a single book (used after split)
#[derive(Clone)]
struct AnalyzeTask {
    book_id: u32,
}

#[async_trait]
impl Task<BookAnalysisState> for AnalyzeTask {
    async fn run(&self, store: &MemoryStore) -> Result<TaskResult<BookAnalysisState>, CanoError> {
        let book: Book = store
            .get(&format!("book_{}", self.book_id))
            .map_err(|e| CanoError::task_execution(format!("Book not found: {e}")))?;

        let analysis = analyze_prepositions(&book);

        println!(
            "  🔍 [{}] Analyzed '{}': {} prepositions",
            analysis.batch_name, analysis.title, analysis.preposition_count
        );

        // Store analysis
        store.put(&format!("analysis_{}", self.book_id), analysis)?;

        Ok(TaskResult::Single(BookAnalysisState::SummarizeBatch))
    }
}

/// Task: Collect all analyses and create batch summary
#[derive(Clone)]
struct SummarizeBatchTask {
    global_results: GlobalResults,
}

#[async_trait]
impl Task<BookAnalysisState> for SummarizeBatchTask {
    async fn run(&self, store: &MemoryStore) -> Result<TaskResult<BookAnalysisState>, CanoError> {
        let batch_name: String = store.get("batch_name")?;
        let books: Vec<BookMetadata> = store.get("book_metadata")?;

        println!("  📊 [{batch_name}] Summarizing batch results...");

        // Collect all analyses
        let mut analyses = Vec::new();
        for (book_id, _, _) in &books {
            if let Ok(analysis) = store.get::<BookAnalysis>(&format!("analysis_{}", book_id)) {
                analyses.push(analysis);
            }
        }

        if analyses.is_empty() {
            return Err(CanoError::task_execution("No analyses found for batch"));
        }

        // Calculate batch statistics
        let total_books = analyses.len();
        let avg_prepositions = analyses
            .iter()
            .map(|a| a.preposition_count as f64)
            .sum::<f64>()
            / total_books as f64;

        // Collect all unique prepositions across batch
        let mut all_prepositions = HashSet::new();
        for analysis in &analyses {
            all_prepositions.extend(analysis.unique_prepositions.iter().cloned());
        }

        let summary = BatchSummary {
            batch_name: batch_name.clone(),
            total_books,
            avg_prepositions,
            total_unique_prepositions: all_prepositions.len(),
            book_analyses: analyses,
        };

        println!(
            "  ✅ [{batch_name}] Batch complete: {total_books} books, avg {avg_prepositions:.1} prepositions"
        );

        // Add to global results
        self.global_results.add_batch(summary).await;

        Ok(TaskResult::Single(BookAnalysisState::Complete))
    }
}

/// Create a workflow for a specific batch
fn create_batch_workflow(
    store: MemoryStore,
    batch_name: String,
    books: Vec<BookMetadata>,
    global_results: GlobalResults,
) -> Workflow<BookAnalysisState> {
    Workflow::new(store)
        .register(
            BookAnalysisState::Start,
            InitBatchTask {
                batch_name: batch_name.clone(),
                books: books.clone(),
            },
        )
        // Split: Download all books in parallel
        .register_split(
            BookAnalysisState::DownloadBatch,
            books
                .iter()
                .map(|(id, title, url)| DownloadTask {
                    book_id: *id,
                    title: title.clone(),
                    url: url.clone(),
                    batch_name: batch_name.clone(),
                })
                .collect::<Vec<_>>(),
            JoinConfig::new(
                JoinStrategy::All,
                BookAnalysisState::AnalyzeBatch,
            )
            .with_timeout(Duration::from_secs(120)),
        )
        // Split: Analyze all books in parallel
        .register_split(
            BookAnalysisState::AnalyzeBatch,
            books
                .iter()
                .map(|(id, _, _)| AnalyzeTask { book_id: *id })
                .collect::<Vec<_>>(),
            JoinConfig::new(
                JoinStrategy::Percentage(0.75), // Proceed if 75% complete
                BookAnalysisState::SummarizeBatch,
            )
            .with_timeout(Duration::from_secs(60)),
        )
        .register(
            BookAnalysisState::SummarizeBatch,
            SummarizeBatchTask {
                global_results: global_results.clone(),
            },
        )
        .add_exit_states(vec![BookAnalysisState::Complete, BookAnalysisState::Error])
}

/// Reduce: Aggregate all batch results and display global rankings
async fn reduce_global_results(global_results: &GlobalResults) -> Result<(), CanoError> {
    println!("\n🌐 GLOBAL REDUCE: Aggregating results from all batches");
    println!("{}", "=".repeat(60));

    let batches = global_results.get_all_batches().await;

    if batches.is_empty() {
        return Err(CanoError::task_execution("No batches completed successfully"));
    }

    // Collect all book analyses
    let mut all_books: Vec<BookAnalysis> = batches
        .iter()
        .flat_map(|b| b.book_analyses.clone())
        .collect();

    // Sort by preposition count
    all_books.sort_by(|a, b| b.preposition_count.cmp(&a.preposition_count));

    // Display batch summaries
    println!("\n📦 Batch Summaries:");
    println!("{}", "-".repeat(60));
    for batch in &batches {
        println!("  Batch: {}", batch.batch_name);
        println!("    • Books processed: {}", batch.total_books);
        println!(
            "    • Avg prepositions: {:.1}",
            batch.avg_prepositions
        );
        println!(
            "    • Total unique prepositions: {}",
            batch.total_unique_prepositions
        );
    }

    // Display global rankings
    println!("\n🏆 Global Book Rankings (Top 10):");
    println!("{}", "-".repeat(60));
    for (rank, book) in all_books.iter().take(10).enumerate() {
        println!(
            "  #{}: {} [{}]",
            rank + 1,
            book.title,
            book.batch_name
        );
        println!(
            "      {} unique prepositions | {} total words",
            book.preposition_count, book.total_words
        );
    }

    // Global statistics
    let total_books = all_books.len();
    let avg_prepositions = all_books
        .iter()
        .map(|b| b.preposition_count as f64)
        .sum::<f64>()
        / total_books as f64;

    let mut all_unique_prepositions = HashSet::new();
    for book in &all_books {
        all_unique_prepositions.extend(book.unique_prepositions.iter().cloned());
    }

    println!("\n📈 Global Statistics:");
    println!("{}", "-".repeat(60));
    println!("  Total batches processed: {}", batches.len());
    println!("  Total books analyzed: {}", total_books);
    println!("  Average prepositions per book: {:.1}", avg_prepositions);
    println!(
        "  Total unique prepositions found: {}",
        all_unique_prepositions.len()
    );

    if let (Some(top), Some(bottom)) = (all_books.first(), all_books.last()) {
        println!("\n🥇 Most diverse: {} ({} prepositions)", top.title, top.preposition_count);
        println!("🥉 Least diverse: {} ({} prepositions)", bottom.title, bottom.preposition_count);
    }

    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), CanoError> {
    println!("🚀 Multi-Level Map-Reduce Book Analysis");
    println!("{}", "=".repeat(60));
    println!("📚 Level 1: Workflow-level Split/Join (within each batch)");
    println!("🌐 Level 2: Scheduler-level Map-Reduce (across all batches)");
    println!("{}", "=".repeat(60));

    let mut scheduler = Scheduler::new();
    let global_results = GlobalResults::new();

    // Get book batches
    let batches = get_book_batches();
    println!("\n📦 Preparing {} batches for processing\n", batches.len());

    // Create and register a workflow for each batch
    for (batch_name, books) in batches {
        let store = MemoryStore::new();
        let workflow = create_batch_workflow(
            store,
            batch_name.clone(),
            books.clone(),
            global_results.clone(),
        );

        println!(
            "  ✅ Registered workflow: {} ({} books)",
            batch_name,
            books.len()
        );

        scheduler.manual(&batch_name, workflow, BookAnalysisState::Start)?;
    }

    println!("\n🎬 Starting scheduler...\n");

    // Start scheduler in background
    let mut scheduler_clone = scheduler.clone();
    let scheduler_handle = tokio::spawn(async move {
        scheduler_clone.start().await
    });

    // Give scheduler time to initialize
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Trigger all workflows (MAP phase at scheduler level)
    println!("🗺️  MAP PHASE: Triggering all batch workflows...\n");
    for (batch_name, _) in get_book_batches() {
        println!("  ⚡ Triggering: {}", batch_name);
        scheduler.trigger(&batch_name).await?;
    }

    // Wait for all workflows to complete
    println!("\n⏳ Waiting for all workflows to complete...\n");

    // Check workflow statuses
    let mut all_complete = false;
    for attempt in 0..60 {
        // Wait up to 10 minutes total
        tokio::time::sleep(Duration::from_secs(5)).await;

        let workflows = scheduler.list().await;
        let completed_count = workflows
            .iter()
            .filter(|w| w.status == Status::Completed)
            .count();

        println!(
            "  📊 Progress: {}/{} workflows completed (attempt {})",
            completed_count,
            workflows.len(),
            attempt + 1
        );

        if completed_count == workflows.len() {
            all_complete = true;
            println!("  ✅ All workflows completed!");
            break;
        }
    }

    if !all_complete {
        println!("\n⚠️  Warning: Not all workflows completed in time");
    }

    // REDUCE phase: Aggregate results from all batches
    println!("\n🔄 REDUCE PHASE: Aggregating results from all batches...\n");
    reduce_global_results(&global_results).await?;

    // Stop scheduler
    println!("\n🛑 Stopping scheduler...");
    scheduler.stop().await?;

    // Wait for scheduler to finish
    let _ = scheduler_handle.await;

    println!("\n✅ Multi-level map-reduce analysis complete!");

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_book_batches() {
        let batches = get_book_batches();
        assert_eq!(batches.len(), 3);

        for (batch_name, books) in batches {
            assert!(!batch_name.is_empty());
            assert_eq!(books.len(), 4);
        }
    }

    #[tokio::test]
    async fn test_global_results() {
        let global = GlobalResults::new();

        let summary1 = BatchSummary {
            batch_name: "Batch1".to_string(),
            total_books: 2,
            avg_prepositions: 15.5,
            total_unique_prepositions: 20,
            book_analyses: vec![],
        };

        global.add_batch(summary1).await;

        let batches = global.get_all_batches().await;
        assert_eq!(batches.len(), 1);
        assert_eq!(batches[0].batch_name, "Batch1");
    }

    #[test]
    fn test_analyze_prepositions() {
        let book = Book {
            id: 1,
            title: "Test".to_string(),
            content: "The cat sat on the mat with care. It was under the table.".to_string(),
            batch_name: "TestBatch".to_string(),
        };

        let analysis = analyze_prepositions(&book);
        assert!(analysis.preposition_count > 0);
        assert!(analysis.unique_prepositions.contains("on"));
        assert!(analysis.unique_prepositions.contains("with"));
        assert!(analysis.unique_prepositions.contains("under"));
    }
}
