//! # Book Preposition Analysis Example: Project Gutenberg Book Analysis
//!
//! This example demonstrates a sophisticated book analysis workflow that:
//! 1. **BookDownloaderNode**: Downloads 12 popular books from Project Gutenberg in parallel
//! 2. **PrepositionNode**: Analyzes each book to count unique prepositions
//! 3. **BookRankingByPrepositionNode**: Ranks books by their preposition diversity
//!
//! The workflow showcases parallel processing, text analysis, and book analysis patterns
//! using the Cano framework with Workflow orchestration supporting different node types.
//!
//! ## Execution Modes
//!
//! - **Default**: Workflow orchestration with real downloads
//!   ```bash
//!   cargo run --example book_prepositions
//!   ```
//!
//! - **Mock Mode**: Offline testing with simulated data
//!   ```bash
//!   CANO_MOCK_MODE=1 cargo run --example book_prepositions
//!   ```

use async_trait::async_trait;
use cano::error::CanoError;
use cano::prelude::*;
use cano::store::{MemoryStore, Store};
use futures::future::join_all;
use std::collections::{HashMap, HashSet};
use tokio::time::{Duration, timeout};

/// Result type for workflow workflow control
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum BookPrepositionAction {
    Complete,
    Error,
    Download,
    Analyze,
    Rank,
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

/// Node 1: Downloads books from Project Gutenberg in parallel
struct BookDownloaderNode {
    params: HashMap<String, String>,
}

impl BookDownloaderNode {
    fn new() -> Self {
        Self {
            params: HashMap::new(),
        }
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
    async fn download_book(id: u32, title: String, url: String) -> Result<Book, String> {
        println!("📚 Downloading: {title}");

        let client = reqwest::Client::new();

        // Set a 30-second timeout for each download
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
                return Err(format!(
                    "Content too short for {title}, might be an error page",
                ));
            }

            println!("✅ Downloaded: {title} ({} chars)", content.len());

            Ok(Book {
                id,
                title: title.clone(),
                url,
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
impl Node<BookPrepositionAction> for BookDownloaderNode {
    type PrepResult = Vec<(u32, String, String)>;
    type ExecResult = Vec<Book>;

    fn set_params(&mut self, params: HashMap<String, String>) {
        self.params = params;
    }

    fn config(&self) -> NodeConfig {
        NodeConfig::new().with_fixed_retry(2, Duration::from_secs(1))
    }

    /// Preparation: Get the list of books to download
    async fn prep(&self, _store: &impl Store) -> Result<Self::PrepResult, CanoError> {
        let book_list = Self::get_book_list();
        println!("📋 Prepared {} books for download", book_list.len());
        Ok(book_list)
    }

    /// Execution: Download all books in parallel
    async fn exec(&self, prep_res: Self::PrepResult) -> Self::ExecResult {
        println!(
            "🚀 Starting parallel download of {} books...",
            prep_res.len()
        );

        // Create download futures for all books
        let download_futures: Vec<_> = prep_res
            .into_iter()
            .map(|(id, title, url)| Self::download_book(id, title, url))
            .collect();

        // Execute all downloads in parallel
        let results = join_all(download_futures).await;

        // Collect successful downloads
        let mut books = Vec::new();
        let mut failed_count = 0;

        for result in results {
            match result {
                Ok(book) => books.push(book),
                Err(error) => {
                    eprintln!("❌ Download failed: {error}");
                    failed_count += 1;
                }
            }
        }

        println!(
            "📊 Download summary: {} successful, {} failed",
            books.len(),
            failed_count
        );
        books
    }

    /// Post-processing: Store downloaded books in memory
    async fn post(
        &self,
        store: &impl Store,
        exec_res: Self::ExecResult,
    ) -> Result<BookPrepositionAction, CanoError> {
        if exec_res.is_empty() {
            return Err(CanoError::node_execution(
                "No books were successfully downloaded",
            ));
        }

        store.put("downloaded_books", exec_res.clone())?;

        println!("✅ Stored {} books in memory for analysis", exec_res.len());
        Ok(BookPrepositionAction::Analyze) // Move to analysis phase
    }
}

/// Node 2: Analyzes prepositions in each book
struct PrepositionNode {
    params: HashMap<String, String>,
}

impl PrepositionNode {
    fn new() -> Self {
        Self {
            params: HashMap::new(),
        }
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
impl Node<BookPrepositionAction> for PrepositionNode {
    type PrepResult = Vec<Book>;
    type ExecResult = Vec<BookAnalysis>;

    fn set_params(&mut self, params: HashMap<String, String>) {
        self.params = params;
    }

    fn config(&self) -> NodeConfig {
        NodeConfig::minimal()
    }

    /// Preparation: Load downloaded books from memory
    async fn prep(&self, store: &impl Store) -> Result<Self::PrepResult, CanoError> {
        let books: Vec<Book> = store
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

    /// Post-processing: Store analysis results
    async fn post(
        &self,
        store: &impl Store,
        exec_res: Self::ExecResult,
    ) -> Result<BookPrepositionAction, CanoError> {
        if exec_res.is_empty() {
            return Err(CanoError::node_execution("No book analyses were completed"));
        }

        store.put("book_analyses", exec_res.clone())?;

        // Clean up the raw book content to save memory
        store.delete("downloaded_books")?;

        println!(
            "✅ Stored {} book analyses and cleaned up raw content",
            exec_res.len()
        );
        Ok(BookPrepositionAction::Rank) // Move to ranking phase
    }
}

/// Node 3: Ranks books by their preposition diversity
struct BookRankingByPrepositionNode {
    params: HashMap<String, String>,
}

impl BookRankingByPrepositionNode {
    fn new() -> Self {
        Self {
            params: HashMap::new(),
        }
    }
}

#[async_trait]
impl Node<BookPrepositionAction> for BookRankingByPrepositionNode {
    type PrepResult = Vec<BookAnalysis>;
    type ExecResult = Vec<BookRanking>;

    fn set_params(&mut self, params: HashMap<String, String>) {
        self.params = params;
    }

    fn config(&self) -> NodeConfig {
        NodeConfig::minimal()
    }

    /// Preparation: Load book analyses from memory
    async fn prep(&self, store: &impl Store) -> Result<Self::PrepResult, CanoError> {
        let analyses: Vec<BookAnalysis> = store
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
        store: &impl Store,
        exec_res: Self::ExecResult,
    ) -> Result<BookPrepositionAction, CanoError> {
        store.put("book_rankings", exec_res.clone())?;

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
        Ok(BookPrepositionAction::Complete)
    }
}

/// Book preposition analysis workflow using Workflow orchestration with different node types
async fn run_workflow() -> Result<(), CanoError> {
    println!("🚀 Starting Book Preposition Analysis Workflow with Workflow");
    println!("========================================================");

    let store = MemoryStore::new();

    // Create a Workflow that handles all three different node types
    let mut workflow = Workflow::new(BookPrepositionAction::Download);

    // Register different node types for each phase
    workflow
        .register_node(BookPrepositionAction::Download, BookDownloaderNode::new())
        .register_node(BookPrepositionAction::Analyze, PrepositionNode::new())
        .register_node(
            BookPrepositionAction::Rank,
            BookRankingByPrepositionNode::new(),
        )
        .add_exit_states(vec![
            BookPrepositionAction::Complete,
            BookPrepositionAction::Error,
        ]);

    println!("📋 Workflow configured with 3 different node types:");
    println!("  • BookDownloaderNode (Download phase)");
    println!("  • PrepositionNode (Analysis phase)");
    println!("  • BookRankingByPrepositionNode (Ranking phase)");

    // Execute the entire workflow using Workflow orchestration
    match workflow.orchestrate(&store).await {
        Ok(final_state) => {
            match final_state {
                BookPrepositionAction::Complete => {
                    println!(
                        "\n✅ Workflow-based book preposition analysis workflow completed successfully!"
                    );

                    // Display summary from the final rankings
                    if let Ok(rankings) = store.get::<Vec<BookRanking>>("book_rankings") {
                        println!("\n🏆 WORKFLOW SUMMARY");
                        println!("==================");
                        println!("Total books analyzed: {}", rankings.len());

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
                    }
                }
                BookPrepositionAction::Error => {
                    eprintln!("❌ Workflow terminated with error state");
                    return Err(CanoError::workflow("Workflow terminated with error state"));
                }
                other => {
                    eprintln!("⚠️  Workflow ended in unexpected state: {other:?}");
                    return Err(CanoError::workflow(format!(
                        "Workflow ended in unexpected state: {other:?}"
                    )));
                }
            }
        }
        Err(e) => {
            eprintln!("❌ Workflow-based workflow failed: {e}");
            return Err(e);
        }
    }

    Ok(())
}

#[tokio::main]
async fn main() {
    println!("📚 Project Gutenberg Book Preposition Analysis");
    println!("=========================================");

    println!("🌐 Running with Workflow orchestration");

    match run_workflow().await {
        Ok(()) => {
            println!("\n🎉 Workflow completed successfully!");
        }
        Err(e) => {
            eprintln!("\n❌ Workflow failed: {e}");
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cano::store::{MemoryStore, Store};

    #[tokio::test]
    async fn test_book_downloader_prep() {
        let downloader = BookDownloaderNode::new();
        let store = MemoryStore::new();

        let book_list = downloader.prep(&store).await.unwrap();

        assert_eq!(book_list.len(), 12);
        assert!(book_list.iter().all(|(id, title, url)| {
            *id > 0 && !title.is_empty() && url.starts_with("https://www.gutenberg.org/")
        }));
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

        let analysis = PrepositionNode::analyze_book_prepositions(&book);

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
    async fn test_preposition_node_with_mock_data() {
        let store = MemoryStore::new();

        // Setup mock books
        let mock_books = vec![
            Book {
                id: 1,
                title: "Book One".to_string(),
                url: "http://example.com/1".to_string(),
                content: "The quick brown fox jumps over the lazy dog near the fence.".to_string(),
            },
            Book {
                id: 2,
                title: "Book Two".to_string(),
                url: "http://example.com/2".to_string(),
                content: "Under the bright sky, birds fly above the trees with grace.".to_string(),
            },
        ];

        store.put("downloaded_books", mock_books).unwrap();

        let analyzer = PrepositionNode::new();
        let result = analyzer.run(&store).await.unwrap();

        assert_eq!(result, BookPrepositionAction::Rank);

        // Verify analyses were stored
        let analyses: Vec<BookAnalysis> = store.get("book_analyses").unwrap();
        assert_eq!(analyses.len(), 2);

        for analysis in &analyses {
            assert!(analysis.preposition_count > 0);
            assert!(analysis.total_words > 0);
            assert!(analysis.preposition_density > 0.0);
        }
    }

    #[tokio::test]
    async fn test_ranking_node() {
        let store = MemoryStore::new();

        // Setup mock analyses
        let mock_analyses = vec![
            BookAnalysis {
                book_id: 1,
                title: "Book A".to_string(),
                preposition_count: 15,
                total_words: 1000,
                preposition_density: 1.5,
                unique_prepositions: HashSet::new(),
            },
            BookAnalysis {
                book_id: 2,
                title: "Book B".to_string(),
                preposition_count: 20,
                total_words: 1200,
                preposition_density: 1.7,
                unique_prepositions: HashSet::new(),
            },
            BookAnalysis {
                book_id: 3,
                title: "Book C".to_string(),
                preposition_count: 10,
                total_words: 800,
                preposition_density: 1.25,
                unique_prepositions: HashSet::new(),
            },
        ];

        store.put("book_analyses", mock_analyses).unwrap();

        let ranker = BookRankingByPrepositionNode::new();
        let result = ranker.run(&store).await.unwrap();

        assert_eq!(result, BookPrepositionAction::Complete);

        // Verify rankings were stored and properly sorted
        let rankings: Vec<BookRanking> = store.get("book_rankings").unwrap();
        assert_eq!(rankings.len(), 3);

        // Should be sorted by preposition count (descending)
        assert_eq!(rankings[0].rank, 1);
        assert_eq!(rankings[0].analysis.title, "Book B"); // 20 prepositions
        assert_eq!(rankings[1].rank, 2);
        assert_eq!(rankings[1].analysis.title, "Book A"); // 15 prepositions
        assert_eq!(rankings[2].rank, 3);
        assert_eq!(rankings[2].analysis.title, "Book C"); // 10 prepositions
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

    #[test]
    fn test_book_list() {
        let book_list = BookDownloaderNode::get_book_list();

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
}
