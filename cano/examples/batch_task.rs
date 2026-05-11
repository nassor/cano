//! # BatchTask — Fan-Out-Over-Data Processing Model
//!
//! Run with: `cargo run --example batch_task`
//!
//! Demonstrates three progressively-richer [`BatchTask`] shapes, all in one workflow:
//!
//! 1. **`ParseUrls`** — parse and validate URL strings in parallel; fail fast on
//!    unrecognised schemes while tolerating other parse errors.
//! 2. **`FetchUrls`** — "fetch" each valid URL concurrently (simulated with a
//!    50 ms sleep); `item_retry()` retries each item up to twice so transient
//!    failures don't abort the whole batch.
//! 3. **`Summarise`** — aggregate fetch results; at least 50% must succeed,
//!    otherwise the whole task fails.
//!
//! Workflow shape:
//!
//! ```text
//!   ParseUrls ──► FetchUrls ──► Summarise ──► Done
//! ```
//!
//! Key BatchTask concepts shown:
//! - `concurrency()` to bound parallel `process_item` calls
//! - `item_retry()` for per-item transient-failure recovery
//! - Partial-failure tolerance in `finish`
//! - Passing data between states via [`MemoryStore`]

use std::time::Duration;

use cano::prelude::*;

// ---------------------------------------------------------------------------
// State enum
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Step {
    /// Fan-out: validate the input URL list.
    ParseUrls,
    /// Fan-out: fetch each valid URL concurrently.
    FetchUrls,
    /// Fan-out: aggregate fetch results and decide final status.
    Summarise,
    /// Terminal exit state.
    Done,
}

// ---------------------------------------------------------------------------
// Resource key enum
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Key {
    Store,
}

// ---------------------------------------------------------------------------
// Task 1: ParseUrls — validate URL strings
// ---------------------------------------------------------------------------

/// Validate URL strings in parallel. Unsupported schemes (`ftp://`, etc.) are
/// fatal — the task returns `Err` immediately. Malformed URLs that might be
/// valid under a looser definition are tolerated: they produce an `Err` slot in
/// `outputs` that `finish` counts and either accepts or rejects.
struct ParseUrls;

#[batch_task(state = Step, key = Key)]
impl ParseUrls {
    fn concurrency(&self) -> usize {
        4
    }

    async fn load(&self, res: &Resources<Key>) -> Result<Vec<String>, CanoError> {
        let store = res.get::<MemoryStore, _>(&Key::Store)?;
        let urls: Vec<String> = store.get("input_urls")?;
        println!("parse urls : loaded {} URLs", urls.len());
        Ok(urls)
    }

    async fn process_item(&self, item: &String) -> Result<String, CanoError> {
        // Reject unsupported schemes immediately (these become batch-level errors
        // when finish receives an Err slot and decides the policy).
        if item.starts_with("ftp://") {
            return Err(CanoError::task_execution(format!(
                "unsupported scheme in {item}"
            )));
        }
        // Simulate very light parsing work.
        if item.starts_with("http://") || item.starts_with("https://") {
            Ok(item.clone())
        } else {
            Err(CanoError::task_execution(format!(
                "cannot parse URL: {item}"
            )))
        }
    }

    async fn finish(
        &self,
        res: &Resources<Key>,
        outputs: Vec<Result<String, CanoError>>,
    ) -> Result<TaskResult<Step>, CanoError> {
        let valid: Vec<String> = outputs.into_iter().filter_map(|r| r.ok()).collect();
        println!("parse urls : {} valid URLs after validation", valid.len());

        if valid.is_empty() {
            return Err(CanoError::task_execution("no valid URLs to fetch"));
        }

        let store = res.get::<MemoryStore, _>(&Key::Store)?;
        store.put("valid_urls", valid)?;
        Ok(TaskResult::Single(Step::FetchUrls))
    }
}

// ---------------------------------------------------------------------------
// Task 2: FetchUrls — fetch each URL concurrently with per-item retry
// ---------------------------------------------------------------------------

/// Simulate fetching each URL. Up to 3 concurrent fetches; each item retries
/// twice on failure so transient network blips don't abort the whole batch.
struct FetchUrls;

#[batch_task(state = Step, key = Key)]
impl FetchUrls {
    fn concurrency(&self) -> usize {
        3
    }

    fn item_retry(&self) -> RetryMode {
        RetryMode::fixed(2, Duration::from_millis(5))
    }

    async fn load(&self, res: &Resources<Key>) -> Result<Vec<String>, CanoError> {
        let store = res.get::<MemoryStore, _>(&Key::Store)?;
        let urls: Vec<String> = store.get("valid_urls")?;
        println!(
            "fetch urls : fetching {} URLs (concurrency=3, retry=2)",
            urls.len()
        );
        Ok(urls)
    }

    async fn process_item(&self, item: &String) -> Result<(String, usize), CanoError> {
        // Simulate a fetch: 50ms latency + a byte count.
        tokio::time::sleep(Duration::from_millis(50)).await;
        let bytes = item.len() * 100; // placeholder "response size"
        println!("fetch urls : fetched {item} → {bytes} bytes");
        Ok((item.clone(), bytes))
    }

    async fn finish(
        &self,
        res: &Resources<Key>,
        outputs: Vec<Result<(String, usize), CanoError>>,
    ) -> Result<TaskResult<Step>, CanoError> {
        let results: Vec<(String, usize)> = outputs.into_iter().filter_map(|r| r.ok()).collect();
        println!("fetch urls : {} URLs fetched successfully", results.len());

        let store = res.get::<MemoryStore, _>(&Key::Store)?;
        store.put("fetch_results", results)?;
        Ok(TaskResult::Single(Step::Summarise))
    }
}

// ---------------------------------------------------------------------------
// Task 3: Summarise — aggregate and decide final state
// ---------------------------------------------------------------------------

/// Aggregate all fetch results. The policy here: at least 50 % of the original
/// URLs must have been fetched successfully or the task fails.
struct Summarise {
    url_count: usize,
}

#[batch_task(state = Step, key = Key)]
impl Summarise {
    async fn load(&self, res: &Resources<Key>) -> Result<Vec<(String, usize)>, CanoError> {
        let store = res.get::<MemoryStore, _>(&Key::Store)?;
        let results: Vec<(String, usize)> = store.get("fetch_results")?;
        Ok(results)
    }

    async fn process_item(&self, item: &(String, usize)) -> Result<usize, CanoError> {
        // Each item just extracts its byte count; the "processing" is trivial here.
        Ok(item.1)
    }

    async fn finish(
        &self,
        _res: &Resources<Key>,
        outputs: Vec<Result<usize, CanoError>>,
    ) -> Result<TaskResult<Step>, CanoError> {
        let total_bytes: usize = outputs.iter().filter_map(|r| r.as_ref().ok()).sum();
        let success_count = outputs.iter().filter(|r| r.is_ok()).count();
        let pct = (success_count * 100)
            .checked_div(self.url_count)
            .unwrap_or(0);
        println!(
            "summarise  : {success_count}/{} succeeded ({pct}%), {total_bytes} bytes total",
            self.url_count
        );

        if pct < 50 {
            return Err(CanoError::task_execution(format!(
                "only {pct}% of URLs succeeded — below the 50% threshold"
            )));
        }

        Ok(TaskResult::Single(Step::Done))
    }
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() -> CanoResult<()> {
    let input_urls = vec![
        "https://example.com/page1".to_string(),
        "https://example.com/page2".to_string(),
        "https://example.com/page3".to_string(),
        "http://legacy.example.com/api".to_string(),
        "ftp://old-server.example.com/data".to_string(), // will fail parsing
        "not-a-url".to_string(),                         // will fail parsing
    ];

    println!("=== batch_task example ===");
    println!("input: {} URL strings\n", input_urls.len());

    let store = MemoryStore::new();
    store.put("input_urls", input_urls.clone())?;

    let resources = Resources::<Key>::new().insert(Key::Store, store);

    let url_count = input_urls.len();
    let workflow = Workflow::new(resources)
        .register(Step::ParseUrls, ParseUrls)
        .register(Step::FetchUrls, FetchUrls)
        .register(Step::Summarise, Summarise { url_count })
        .add_exit_state(Step::Done);

    let result = workflow.orchestrate(Step::ParseUrls).await?;
    assert_eq!(result, Step::Done);
    println!("\ncompleted at {result:?}");

    Ok(())
}
