//! # Book Preposition Analysis with Scheduler: Concurrent Downloads + Sequential Analysis
//!
//! This example demonstrates a sophisticated book analysis system that combines:
//! 1. **Concurrent Workflow**: Downloads 12 popular books from Project Gutenberg in parallel
//! 2. **Regular Workflow**: Sequential analysis and ranking pipeline
//! 3. **Scheduler**: Orchestrates the connection between workflows
//!
//! The workflow architecture showcases:
//! - Concurrent downloading for maximum throughput using queue-based work distribution
//! - Sequential analysis pipeline for data consistency
//! - Scheduler-based workflow orchestration and coordination
//! - Shared store for data transfer between workflow types
//!
//! ## Execution
//!
//! ```bash
//! cargo run --example scheduler_book_prepositions
//! ```

use async_trait::async_trait;
use cano::error::CanoError;
use cano::prelude::*;
use cano::store::{KeyValueStore, MemoryStore};
use futures::future::join_all;
use std::collections::HashSet;
use std::sync::Arc;
use tokio::sync::RwLock;
use tokio::time::{Duration, timeout};

/// Workflow state management for scheduler coordination
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum WorkflowPhase {
    // Download workflow states
    Download,
    DownloadComplete,
    DownloadError,

    // Analysis workflow states
    Analyze,
    Rank,
    Complete,
    Error,
}

/// Book data structure
#[derive(Debug, Clone)]
struct Book {
    id: u32,
    title: String,
    #[allow(dead_code)]
    url: String,
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
    unique_prepositions: HashSet<String>,
}

/// Book ranking result
#[derive(Debug, Clone)]
struct BookRanking {
    rank: usize,
    analysis: BookAnalysis,
}

/// Common English prepositions for analysis
const PREPOSITIONS: &[&str] = &[
    "aboard",
    "about",
    "above",
    "across",
    "after",
    "against",
    "along",
    "amid",
    "among",
    "around",
    "as",
    "at",
    "before",
    "behind",
    "below",
    "beneath",
    "beside",
    "between",
    "beyond",
    "by",
    "concerning",
    "considering",
    "despite",
    "down",
    "during",
    "except",
    "excepting",
    "excluding",
    "following",
    "for",
    "from",
    "in",
    "inside",
    "into",
    "like",
    "minus",
    "near",
    "of",
    "off",
    "on",
    "onto",
    "opposite",
    "outside",
    "over",
    "past",
    "per",
    "plus",
    "regarding",
    "round",
    "save",
    "since",
    "than",
    "through",
    "to",
    "toward",
    "towards",
    "under",
    "underneath",
    "unlike",
    "until",
    "up",
    "upon",
    "versus",
    "via",
    "with",
    "within",
    "without",
];

/// Global shared store for inter-workflow communication
type SharedStore = Arc<RwLock<MemoryStore>>;

/// Download Node: Downloads a book from the queue in shared store
#[derive(Clone)]
struct BookDownloadNode {
    shared_store: SharedStore,
}

impl BookDownloadNode {
    fn new(shared_store: SharedStore) -> Self {
        Self { shared_store }
    }

    /// List of popular Project Gutenberg books with their plain text URLs
    fn get_book_list() -> Vec<(u32, String, String)> {
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
            (
                2701,
                "Moby Dick by Herman Melville".to_string(),
                "https://www.gutenberg.org/files/2701/2701-0.txt".to_string(),
            ),
            (
                1080,
                "A Modest Proposal by Jonathan Swift".to_string(),
                "https://www.gutenberg.org/files/1080/1080-0.txt".to_string(),
            ),
            (
                46,
                "A Christmas Carol by Charles Dickens".to_string(),
                "https://www.gutenberg.org/files/46/46-0.txt".to_string(),
            ),
            (
                1513,
                "Romeo and Juliet by William Shakespeare".to_string(),
                "https://www.gutenberg.org/files/1513/1513-0.txt".to_string(),
            ),
            (
                174,
                "The Picture of Dorian Gray by Oscar Wilde".to_string(),
                "https://www.gutenberg.org/files/174/174-0.txt".to_string(),
            ),
            (
                345,
                "Dracula by Bram Stoker".to_string(),
                "https://www.gutenberg.org/files/345/345-0.txt".to_string(),
            ),
            (
                76,
                "Adventures of Huckleberry Finn by Mark Twain".to_string(),
                "https://www.gutenberg.org/files/76/76-0.txt".to_string(),
            ),
            (
                1260,
                "Jane Eyre by Charlotte Brontë".to_string(),
                "https://www.gutenberg.org/files/1260/1260-0.txt".to_string(),
            ),
        ]
    }

    /// Download a single book with error handling and timeout
    async fn download_book(book_id: u32, title: &str, url: &str) -> Result<Book, String> {
        println!("📚 Downloading: {title}");

        let client = reqwest::Client::new();

        // Set a 30-second timeout for each download
        let download_future = async {
            let response = client
                .get(url)
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
                return Err(format!(
                    "Content too short for {title}, might be an error page",
                ));
            }

            println!("✅ Downloaded: {title} ({} chars)", content.len());

            Ok(Book {
                id: book_id,
                title: title.to_string(),
                url: url.to_string(),
                content,
            })
        };

        match timeout(Duration::from_secs(30), download_future).await {
            Ok(result) => result,
            Err(_) => Err(format!("Timeout downloading {title}")),
        }
    }
}

#[async_trait]
impl Node<WorkflowPhase> for BookDownloadNode {
    type PrepResult = Option<(u32, String, String)>;
    type ExecResult = Option<Book>;

    fn config(&self) -> NodeConfig {
        NodeConfig::new().with_fixed_retry(2, Duration::from_secs(1))
    }

    /// Preparation: Get next book from the download queue
    async fn prep(&self, _store: &MemoryStore) -> Result<Self::PrepResult, CanoError> {
        let shared_store = self.shared_store.write().await;

        // Get the download queue
        let mut queue: Vec<(u32, String, String)> = shared_store
            .get("download_queue")
            .unwrap_or_else(|_| Vec::new());

        if queue.is_empty() {
            // No more books to download
            return Ok(None);
        }

        // Take the first book from the queue
        let book_info = queue.remove(0);
        shared_store.put("download_queue", queue)?;

        println!("📋 Picked up for download: {}", book_info.1);
        Ok(Some(book_info))
    }

    /// Execution: Download the book if one was assigned
    async fn exec(&self, prep_res: Self::PrepResult) -> Self::ExecResult {
        if let Some((book_id, title, url)) = prep_res {
            match Self::download_book(book_id, &title, &url).await {
                Ok(book) => Some(book),
                Err(error) => {
                    eprintln!("❌ Download failed for {title}: {error}");
                    None
                }
            }
        } else {
            println!("📭 No more books to download");
            None
        }
    }

    /// Post-processing: Store the book in the shared store
    async fn post(
        &self,
        _store: &MemoryStore,
        exec_res: Self::ExecResult,
    ) -> Result<WorkflowPhase, CanoError> {
        if let Some(book) = exec_res {
            // Store the book in the shared store
            let shared_store = self.shared_store.write().await;

            // Get existing books or create new vector
            let mut books: Vec<Book> = shared_store
                .get("downloaded_books")
                .unwrap_or_else(|_| Vec::new());

            books.push(book.clone());
            shared_store.put("downloaded_books", books)?;

            println!("✅ Stored book: {} in shared store", book.title);
            Ok(WorkflowPhase::DownloadComplete)
        } else {
            Ok(WorkflowPhase::DownloadError)
        }
    }
}

/// Analysis Node: Analyzes prepositions in downloaded books
#[derive(Clone)]
struct PrepositionAnalysisNode {
    shared_store: SharedStore,
}

impl PrepositionAnalysisNode {
    fn new(shared_store: SharedStore) -> Self {
        Self { shared_store }
    }

    /// Analyze prepositions in a single book
    fn analyze_book_prepositions(book: &Book) -> BookAnalysis {
        let preposition_set: HashSet<&str> = PREPOSITIONS.iter().copied().collect();
        let mut found_prepositions = HashSet::new();
        let mut total_preposition_count = 0;

        // Clean and tokenize the text
        let content_lower = book.content.to_lowercase();
        let words: Vec<&str> = content_lower
            .split_whitespace()
            .map(|word| {
                // Remove punctuation
                word.trim_matches(|c: char| !c.is_alphabetic())
            })
            .filter(|word| !word.is_empty())
            .collect();

        let total_words = words.len();

        // Count prepositions
        for word in words {
            if preposition_set.contains(word) {
                found_prepositions.insert(word.to_string());
                total_preposition_count += 1;
            }
        }

        let preposition_density = if total_words > 0 {
            (total_preposition_count as f64 / total_words as f64) * 100.0
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
            unique_prepositions: found_prepositions,
        }
    }
}

#[async_trait]
impl Node<WorkflowPhase> for PrepositionAnalysisNode {
    type PrepResult = Vec<Book>;
    type ExecResult = Vec<BookAnalysis>;

    fn config(&self) -> NodeConfig {
        NodeConfig::minimal()
    }

    /// Preparation: Load downloaded books from shared store
    async fn prep(&self, _store: &MemoryStore) -> Result<Self::PrepResult, CanoError> {
        let shared_store = self.shared_store.read().await;
        let books: Vec<Book> = shared_store
            .get("downloaded_books")
            .map_err(|e| CanoError::preparation(format!("Failed to load books: {e}")))?;

        println!("📚 Loaded {} books for preposition analysis", books.len());
        Ok(books)
    }

    /// Execution: Analyze prepositions in all books
    async fn exec(&self, prep_res: Self::PrepResult) -> Self::ExecResult {
        println!("🔍 Analyzing prepositions in {} books...", prep_res.len());

        // Process books in parallel using async tasks
        let analysis_futures: Vec<_> = prep_res
            .iter()
            .map(|book| {
                let book_clone = book.clone();
                tokio::spawn(async move { Self::analyze_book_prepositions(&book_clone) })
            })
            .collect();

        let results = join_all(analysis_futures).await;

        let mut analyses = Vec::new();
        for result in results {
            match result {
                Ok(analysis) => analyses.push(analysis),
                Err(e) => eprintln!("❌ Analysis task failed: {e}"),
            }
        }

        println!("📊 Completed analysis of {} books", analyses.len());
        analyses
    }

    /// Post-processing: Store analysis results in shared store
    async fn post(
        &self,
        _store: &MemoryStore,
        exec_res: Self::ExecResult,
    ) -> Result<WorkflowPhase, CanoError> {
        if exec_res.is_empty() {
            return Err(CanoError::node_execution("No book analyses were completed"));
        }

        let shared_store = self.shared_store.read().await;
        shared_store.put("book_analyses", exec_res.clone())?;

        // Clean up the raw book content to save memory
        shared_store.remove("downloaded_books")?;

        println!(
            "✅ Stored {} book analyses and cleaned up raw content",
            exec_res.len()
        );
        Ok(WorkflowPhase::Rank) // Move to ranking phase
    }
}

/// Ranking Node: Ranks books by their preposition diversity
#[derive(Clone)]
struct BookRankingNode {
    shared_store: SharedStore,
}

impl BookRankingNode {
    fn new(shared_store: SharedStore) -> Self {
        Self { shared_store }
    }
}

#[async_trait]
impl Node<WorkflowPhase> for BookRankingNode {
    type PrepResult = Vec<BookAnalysis>;
    type ExecResult = Vec<BookRanking>;

    fn config(&self) -> NodeConfig {
        NodeConfig::minimal()
    }

    /// Preparation: Load book analyses from shared store
    async fn prep(&self, _store: &MemoryStore) -> Result<Self::PrepResult, CanoError> {
        let shared_store = self.shared_store.read().await;
        let analyses: Vec<BookAnalysis> = shared_store
            .get("book_analyses")
            .map_err(|e| CanoError::preparation(format!("Failed to load analyses: {e}")))?;

        println!("📊 Loaded {} book analyses for ranking", analyses.len());
        Ok(analyses)
    }

    /// Execution: Rank books by preposition count and density
    async fn exec(&self, prep_res: Self::PrepResult) -> Self::ExecResult {
        println!("🏆 Ranking books by preposition diversity...");

        let mut analyses = prep_res;

        // Sort by preposition count (descending), then by density as tiebreaker
        analyses.sort_by(|a, b| match b.preposition_count.cmp(&a.preposition_count) {
            std::cmp::Ordering::Equal => b
                .preposition_density
                .partial_cmp(&a.preposition_density)
                .unwrap_or(std::cmp::Ordering::Equal),
            other => other,
        });

        // Create rankings
        let rankings: Vec<BookRanking> = analyses
            .into_iter()
            .enumerate()
            .map(|(index, analysis)| BookRanking {
                rank: index + 1,
                analysis,
            })
            .collect();

        println!(
            "📈 Ranked {} books by preposition diversity",
            rankings.len()
        );
        rankings
    }

    /// Post-processing: Store final rankings and display results
    async fn post(
        &self,
        _store: &MemoryStore,
        exec_res: Self::ExecResult,
    ) -> Result<WorkflowPhase, CanoError> {
        let shared_store = self.shared_store.read().await;
        shared_store.put("book_rankings", exec_res.clone())?;

        // Display results
        println!("\n🏆 FINAL BOOK RANKINGS BY PREPOSITION DIVERSITY");
        println!("================================================");

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

            // Show some example prepositions
            let sample_prepositions: Vec<&String> = ranking
                .analysis
                .unique_prepositions
                .iter()
                .take(10)
                .collect();
            println!("    🔤 Sample prepositions: {sample_prepositions:?}");
            println!();
        }

        // Summary statistics
        let total_books = exec_res.len();
        let avg_prepositions: f64 = exec_res
            .iter()
            .map(|r| r.analysis.preposition_count as f64)
            .sum::<f64>()
            / total_books as f64;
        let avg_density: f64 = exec_res
            .iter()
            .map(|r| r.analysis.preposition_density)
            .sum::<f64>()
            / total_books as f64;

        println!("📈 SUMMARY STATISTICS");
        println!("=====================");
        println!("Total books analyzed: {}", total_books);
        println!(
            "Average unique prepositions per book: {:.1}",
            avg_prepositions
        );
        println!("Average preposition density: {:.2}%", avg_density);

        if let (Some(highest), Some(lowest)) = (exec_res.first(), exec_res.last()) {
            println!(
                "\n🥇 Highest diversity: {} ({} prepositions)",
                highest.analysis.title, highest.analysis.preposition_count
            );
            println!(
                "🥉 Lowest diversity: {} ({} prepositions)",
                lowest.analysis.title, lowest.analysis.preposition_count
            );
        }

        println!("✅ Book preposition analysis complete!");
        Ok(WorkflowPhase::Complete)
    }
}

/// Create the concurrent download workflow
fn create_download_workflow(shared_store: SharedStore) -> ConcurrentWorkflow<WorkflowPhase> {
    let mut concurrent_workflow = ConcurrentWorkflow::new(WorkflowPhase::Download);

    // Register the download node (will be cloned for each instance)
    concurrent_workflow
        .register_node(WorkflowPhase::Download, BookDownloadNode::new(shared_store))
        .add_exit_states(vec![
            WorkflowPhase::DownloadComplete,
            WorkflowPhase::DownloadError,
        ]);

    concurrent_workflow
}

/// Create the sequential analysis workflow
fn create_analysis_workflow(shared_store: SharedStore) -> Workflow<WorkflowPhase> {
    let mut workflow = Workflow::new(WorkflowPhase::Analyze);

    workflow
        .register_node(
            WorkflowPhase::Analyze,
            PrepositionAnalysisNode::new(shared_store.clone()),
        )
        .register_node(WorkflowPhase::Rank, BookRankingNode::new(shared_store))
        .add_exit_states(vec![WorkflowPhase::Complete, WorkflowPhase::Error]);

    workflow
}

/// Main scheduler-based book preposition analysis
async fn run_scheduler_based_analysis() -> Result<(), CanoError> {
    println!("🚀 Starting Scheduler-Based Book Preposition Analysis");
    println!("====================================================");
    println!("📋 Architecture:");
    println!("  • Concurrent Workflow: Downloads 12 books in parallel via work queue");
    println!("  • Regular Workflow: Sequential analysis and ranking");
    println!("  • Scheduler: Orchestrates workflow coordination");
    println!();

    // Create shared store for inter-workflow communication
    let shared_store = Arc::new(RwLock::new(MemoryStore::new()));

    // Create scheduler
    let mut scheduler = Scheduler::new();

    // Initialize the download queue in shared store
    {
        let shared_store_guard = shared_store.write().await;
        let book_list = BookDownloadNode::get_book_list();
        shared_store_guard.put("download_queue", book_list)?;
        println!("📋 Initialized download queue with {} books", 12);
    }

    // Create and register concurrent download workflow
    let download_workflow = create_download_workflow(shared_store.clone());

    println!("📥 Registering concurrent download workflow with 12 instances...");
    scheduler.manual_concurrent(
        "book_downloads",
        download_workflow,
        12,                        // One instance per book
        WaitStrategy::WaitForever, // Wait for all downloads to complete
    )?;

    // Create and register sequential analysis workflow
    let analysis_workflow = create_analysis_workflow(shared_store.clone());

    println!("📊 Registering sequential analysis workflow...");
    scheduler.manual("book_analysis", analysis_workflow)?;

    // Start the scheduler
    println!("🎬 Starting scheduler...");
    scheduler.start().await?;

    // Trigger the concurrent download workflow
    println!("\n🔄 Triggering concurrent book download workflow...");
    scheduler.trigger("book_downloads").await?;

    // Wait for downloads to complete
    println!("⏳ Waiting for downloads to complete...");

    // Poll for download completion
    let mut download_completed = false;
    let mut check_count = 0;
    const MAX_CHECKS: usize = 60; // 5 minutes max wait

    while !download_completed && check_count < MAX_CHECKS {
        tokio::time::sleep(Duration::from_secs(5)).await;
        check_count += 1;

        // Check download status
        if let Some(info) = scheduler.status("book_downloads").await {
            match &info.status {
                cano::scheduler::Status::ConcurrentCompleted(status) => {
                    println!(
                        "📊 Download status: {}/{} completed, {}/{} failed",
                        status.completed,
                        status.total_workflows,
                        status.failed,
                        status.total_workflows
                    );
                    if status.completed + status.failed >= status.total_workflows {
                        download_completed = true;
                    }
                }
                cano::scheduler::Status::ConcurrentRunning(status) => {
                    println!(
                        "📥 Downloads in progress: {}/{} completed, {} running",
                        status.completed,
                        status.total_workflows,
                        status.running()
                    );
                }
                _ => {
                    println!("📋 Download status: {:?}", info.status);
                }
            }
        }
    }

    if !download_completed {
        eprintln!("❌ Downloads did not complete within the timeout period");
        return Err(CanoError::workflow("Download timeout"));
    }

    // Check how many books were successfully downloaded
    {
        let shared_store_guard = shared_store.read().await;
        let books: Vec<Book> = shared_store_guard
            .get("downloaded_books")
            .unwrap_or_else(|_| Vec::new());

        println!(
            "✅ Downloads completed! {} books successfully downloaded",
            books.len()
        );

        if books.is_empty() {
            eprintln!("❌ No books were successfully downloaded");
            return Err(CanoError::workflow("No books downloaded"));
        }
    }

    // Now trigger the analysis workflow
    println!("\n🔄 Triggering sequential analysis workflow...");
    scheduler.trigger("book_analysis").await?;

    // Wait for analysis to complete
    println!("⏳ Waiting for analysis to complete...");

    let mut analysis_completed = false;
    check_count = 0;

    while !analysis_completed && check_count < MAX_CHECKS {
        tokio::time::sleep(Duration::from_secs(2)).await;
        check_count += 1;

        // Check analysis status
        if let Some(info) = scheduler.status("book_analysis").await {
            match &info.status {
                cano::scheduler::Status::Idle => {
                    if info.run_count > 0 {
                        analysis_completed = true;
                        println!("✅ Analysis workflow completed successfully!");
                    }
                }
                cano::scheduler::Status::Running => {
                    println!("📊 Analysis workflow is running...");
                }
                cano::scheduler::Status::Failed(error) => {
                    eprintln!("❌ Analysis workflow failed: {error}");
                    return Err(CanoError::workflow(format!("Analysis failed: {error}")));
                }
                _ => {
                    println!("📋 Analysis status: {:?}", info.status);
                }
            }
        }
    }

    if !analysis_completed {
        eprintln!("❌ Analysis did not complete within the timeout period");
        return Err(CanoError::workflow("Analysis timeout"));
    }

    // Display final summary
    {
        let shared_store_guard = shared_store.read().await;
        if let Ok(rankings) = shared_store_guard.get::<Vec<BookRanking>>("book_rankings") {
            println!("\n🏆 SCHEDULER-BASED WORKFLOW SUMMARY");
            println!("===================================");
            println!("📊 Total books processed: {}", rankings.len());

            if let (Some(top), Some(bottom)) = (rankings.first(), rankings.last()) {
                println!(
                    "🥇 Most diverse: {} ({} prepositions)",
                    top.analysis.title, top.analysis.preposition_count
                );
                println!(
                    "🥉 Least diverse: {} ({} prepositions)",
                    bottom.analysis.title, bottom.analysis.preposition_count
                );
            }

            println!("\n✨ Architecture Benefits Demonstrated:");
            println!("  • Concurrent workflow with work queue maximized download throughput");
            println!("  • Sequential analysis ensured data consistency");
            println!("  • Scheduler provided workflow orchestration and coordination");
            println!("  • Shared store enabled seamless data transfer between workflow types");
        }
    }

    // Stop the scheduler
    println!("\n🛑 Stopping scheduler...");
    scheduler.stop().await?;

    Ok(())
}

#[tokio::main]
async fn main() {
    println!("📚 Scheduler-Based Book Preposition Analysis");
    println!("===========================================");
    println!("🏗️ Hybrid Architecture: Concurrent Downloads + Sequential Analysis");
    println!();

    match run_scheduler_based_analysis().await {
        Ok(()) => {
            println!("\n🎉 Scheduler-based book analysis completed successfully!");
        }
        Err(e) => {
            eprintln!("\n❌ Scheduler-based analysis failed: {e}");
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cano::store::{MemoryStore, Store};

    #[tokio::test]
    async fn test_book_list() {
        let book_list = BookDownloadNode::get_book_list();

        assert_eq!(book_list.len(), 12);

        // Verify each book has valid data
        for (id, title, url) in &book_list {
            assert!(*id > 0);
            assert!(!title.is_empty());
            assert!(url.starts_with("https://www.gutenberg.org/"));
            assert!(url.ends_with(".txt"));
        }

        // Verify we have some expected classics
        let titles: Vec<String> = book_list
            .iter()
            .map(|(_, title, _)| title.clone())
            .collect();
        let titles_str = titles.join(" ");

        assert!(titles_str.contains("Pride and Prejudice"));
        assert!(titles_str.contains("Alice's Adventures"));
        assert!(titles_str.contains("Sherlock Holmes"));
        assert!(titles_str.contains("Moby Dick"));
    }

    #[tokio::test]
    async fn test_preposition_analysis() {
        let book = Book {
            id: 1,
            title: "Test Book".to_string(),
            url: "http://example.com".to_string(),
            content:
                "The cat sat on the mat with great care. It was under the table, near the door."
                    .to_string(),
        };

        let analysis = PrepositionAnalysisNode::analyze_book_prepositions(&book);

        assert_eq!(analysis.book_id, 1);
        assert_eq!(analysis.title, "Test Book");
        assert!(analysis.preposition_count > 0);
        assert!(analysis.total_words > 0);
        assert!(analysis.preposition_density > 0.0);

        // Should find prepositions like "on", "with", "under", "near"
        assert!(analysis.unique_prepositions.contains("on"));
        assert!(analysis.unique_prepositions.contains("with"));
        assert!(analysis.unique_prepositions.contains("under"));
        assert!(analysis.unique_prepositions.contains("near"));
    }

    #[tokio::test]
    async fn test_shared_store_communication() {
        let shared_store = Arc::new(RwLock::new(MemoryStore::new()));

        // Simulate download phase
        let download_node = BookDownloadNode::new(shared_store.clone());

        // Test that we can store books in shared store
        let store = MemoryStore::new();
        let book = Book {
            id: 1,
            title: "Test Book".to_string(),
            url: "http://example.com".to_string(),
            content: "Test content with prepositions on and under the table.".to_string(),
        };

        download_node.post(&store, Some(book)).await.unwrap();

        // Test that analysis can read from shared store
        let analysis_node = PrepositionAnalysisNode::new(shared_store.clone());
        let books = analysis_node.prep(&store).await.unwrap();

        assert_eq!(books.len(), 1);
        assert_eq!(books[0].title, "Test Book");
    }

    #[test]
    fn test_preposition_constants() {
        // Verify our preposition list contains common ones
        assert!(PREPOSITIONS.contains(&"in"));
        assert!(PREPOSITIONS.contains(&"on"));
        assert!(PREPOSITIONS.contains(&"at"));
        assert!(PREPOSITIONS.contains(&"by"));
        assert!(PREPOSITIONS.contains(&"for"));
        assert!(PREPOSITIONS.contains(&"with"));
        assert!(PREPOSITIONS.contains(&"to"));
        assert!(PREPOSITIONS.contains(&"from"));

        // Should have a reasonable number of prepositions
        assert!(PREPOSITIONS.len() > 50);
        assert!(PREPOSITIONS.len() < 100);
    }

    #[tokio::test]
    async fn test_download_queue_mechanism() {
        let shared_store = Arc::new(RwLock::new(MemoryStore::new()));

        // Initialize queue
        {
            let shared_guard = shared_store.write().await;
            let test_books = vec![
                (1, "Book 1".to_string(), "http://example.com/1".to_string()),
                (2, "Book 2".to_string(), "http://example.com/2".to_string()),
            ];
            shared_guard.put("download_queue", test_books).unwrap();
        }

        let download_node = BookDownloadNode::new(shared_store.clone());
        let store = MemoryStore::new();

        // First call should get Book 1
        let book1 = download_node.prep(&store).await.unwrap();
        assert!(book1.is_some());
        assert_eq!(book1.unwrap().0, 1);

        // Second call should get Book 2
        let book2 = download_node.prep(&store).await.unwrap();
        assert!(book2.is_some());
        assert_eq!(book2.unwrap().0, 2);

        // Third call should get None (empty queue)
        let no_book = download_node.prep(&store).await.unwrap();
        assert!(no_book.is_none());
    }
}
