//! # Simplified Stream API
//!
//! A simplified stream scheduler that focuses on ease of use while maintaining
//! the core scheduling functionality.
//!
//! ## 🚀 Quick Start
//!
//! ```rust
//! use cano::prelude::*;
//! use cano::stream_simple::Stream;
//! use tokio::time::Duration;
//!
//! #[tokio::main]
//! async fn main() -> CanoResult<()> {
//!     let mut stream: Stream<MyState, MemoryStore> = Stream::new();
//!     
//!     let flow = Flow::new(MyState::Start);
//!     
//!     // Multiple ways to schedule flows:
//!     stream.every_seconds("task1", flow, 30)?;                    // Every 30 seconds
//!     stream.every_minutes("task2", flow, 5)?;                     // Every 5 minutes  
//!     stream.every_hours("task3", flow, 2)?;                       // Every 2 hours
//!     stream.every("task4", flow, Duration::from_millis(500))?;    // Every 500ms
//!     stream.cron("task5", flow, "0 */10 * * * *")?;              // Every 10 minutes (cron)
//!     stream.manual("task6", flow)?;                               // Manual trigger only
//!     
//!     stream.start().await?;
//!     Ok(())
//! }
//! ```

use crate::error::{CanoError, CanoResult};
use crate::flow::Flow;
use crate::store::Store;
use chrono::{DateTime, Utc};
use cron::Schedule as CronSchedule;
use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::{RwLock, mpsc};
use tokio::time::{Duration, sleep};

/// Simplified scheduling options
#[derive(Debug, Clone)]
pub enum Schedule {
    /// Run every Duration interval
    Every(Duration),
    /// Cron expression (for advanced users)
    Cron(String),
    /// Manual trigger only
    Manual,
}

/// Simple flow status
#[derive(Debug, Clone, PartialEq)]
pub enum Status {
    Idle,
    Running,
    Failed(String),
}

/// Minimal flow information
#[derive(Debug, Clone)]
pub struct FlowInfo {
    pub id: String,
    pub status: Status,
    pub run_count: u64,
    pub last_run: Option<DateTime<Utc>>,
}

/// Type alias for the complex flow data stored in the stream
type FlowData<T, S> = (Arc<Flow<T, S>>, Schedule, Arc<RwLock<FlowInfo>>);

/// Simplified stream scheduler
pub struct Stream<T, S>
where
    T: Clone + Send + Sync + 'static + std::fmt::Debug + std::hash::Hash + Eq,
    S: Store + Clone + Default + 'static,
{
    flows: HashMap<String, FlowData<T, S>>,
    command_tx: Option<mpsc::UnboundedSender<String>>,
    running: Arc<RwLock<bool>>,
}

impl<T, S> Stream<T, S>
where
    T: Clone + Send + Sync + 'static + std::fmt::Debug + std::hash::Hash + Eq,
    S: Store + Clone + Default + 'static,
{
    /// Create a new stream
    pub fn new() -> Self {
        Self {
            flows: HashMap::new(),
            command_tx: None,
            running: Arc::new(RwLock::new(false)),
        }
    }

    /// Add a flow that runs every Duration interval
    pub fn every(&mut self, id: &str, flow: Flow<T, S>, interval: Duration) -> CanoResult<()> {
        self.add_flow(id, flow, Schedule::Every(interval))
    }

    /// Add a flow that runs every N seconds (convenience method)
    pub fn every_seconds(&mut self, id: &str, flow: Flow<T, S>, seconds: u64) -> CanoResult<()> {
        self.every(id, flow, Duration::from_secs(seconds))
    }

    /// Add a flow that runs every N minutes (convenience method)
    pub fn every_minutes(&mut self, id: &str, flow: Flow<T, S>, minutes: u64) -> CanoResult<()> {
        self.every(id, flow, Duration::from_secs(minutes * 60))
    }

    /// Add a flow that runs every N hours (convenience method)
    pub fn every_hours(&mut self, id: &str, flow: Flow<T, S>, hours: u64) -> CanoResult<()> {
        self.every(id, flow, Duration::from_secs(hours * 3600))
    }

    /// Add a flow with cron schedule
    pub fn cron(&mut self, id: &str, flow: Flow<T, S>, expr: &str) -> CanoResult<()> {
        // Validate cron expression
        CronSchedule::from_str(expr)
            .map_err(|e| CanoError::Configuration(format!("Invalid cron expression: {e}")))?;
        self.add_flow(id, flow, Schedule::Cron(expr.to_string()))
    }

    /// Add a manually triggered flow
    pub fn manual(&mut self, id: &str, flow: Flow<T, S>) -> CanoResult<()> {
        self.add_flow(id, flow, Schedule::Manual)
    }

    /// Internal method to add flows
    fn add_flow(&mut self, id: &str, flow: Flow<T, S>, schedule: Schedule) -> CanoResult<()> {
        if self.flows.contains_key(id) {
            return Err(CanoError::Configuration(format!(
                "Flow '{id}' already exists"
            )));
        }

        let info = FlowInfo {
            id: id.to_string(),
            status: Status::Idle,
            run_count: 0,
            last_run: None,
        };

        self.flows.insert(
            id.to_string(),
            (Arc::new(flow), schedule, Arc::new(RwLock::new(info))),
        );
        Ok(())
    }

    /// Start the scheduler
    pub async fn start(&mut self) -> CanoResult<()> {
        if *self.running.read().await {
            return Err(CanoError::Configuration("Already running".to_string()));
        }

        let (tx, mut rx) = mpsc::unbounded_channel();
        self.command_tx = Some(tx);
        *self.running.write().await = true;

        // Clone data for the scheduler task
        let flows = self.flows.clone();
        let running = Arc::clone(&self.running);

        tokio::spawn(async move {
            let mut last_check = HashMap::new();
            let mut interval = tokio::time::interval(Duration::from_secs(1));

            loop {
                tokio::select! {
                    _ = interval.tick() => {
                        if !*running.read().await {
                            break;
                        }

                        let now = Utc::now();
                        for (id, (flow, schedule, info)) in &flows {
                            if should_run(schedule, &mut last_check, id, now) {
                                let flow = Arc::clone(flow);
                                let info = Arc::clone(info);
                                let store = S::default();

                                tokio::spawn(async move {
                                    execute_flow(flow, info, store).await;
                                });
                            }
                        }
                    }

                    command = rx.recv() => {
                        match command {
                            Some(flow_id) => {
                                if let Some((flow, _, info)) = flows.get(&flow_id) {
                                    let flow = Arc::clone(flow);
                                    let info = Arc::clone(info);
                                    let store = S::default();

                                    tokio::spawn(async move {
                                        execute_flow(flow, info, store).await;
                                    });
                                }
                            }
                            None => break,
                        }
                    }
                }
            }
        });

        Ok(())
    }

    /// Trigger a flow manually
    pub async fn trigger(&self, id: &str) -> CanoResult<()> {
        if let Some(tx) = &self.command_tx {
            tx.send(id.to_string())
                .map_err(|_| CanoError::flow("Failed to trigger flow"))?;
            Ok(())
        } else {
            Err(CanoError::Configuration("Stream not running".to_string()))
        }
    }

    /// Stop the scheduler
    pub async fn stop(&mut self) -> CanoResult<()> {
        self.stop_with_timeout(Duration::from_secs(30)).await
    }

    /// Stop the scheduler with a timeout for waiting on running flows
    pub async fn stop_with_timeout(&mut self, timeout: Duration) -> CanoResult<()> {
        // Signal scheduler to stop accepting new tasks
        *self.running.write().await = false;
        self.command_tx = None;

        // Wait for all running flows to complete
        let start_time = Instant::now();

        loop {
            // Check if any flows are still running
            let mut any_running = false;
            for (_, _, info) in self.flows.values() {
                let status = &info.read().await.status;
                if *status == Status::Running {
                    any_running = true;
                    break;
                }
            }

            // If no flows are running, we're done
            if !any_running {
                break;
            }

            // Check if we've exceeded the timeout
            if start_time.elapsed() >= timeout {
                return Err(CanoError::Configuration(format!(
                    "Timeout after {timeout:?} waiting for flows to complete"
                )));
            }

            // Wait a bit before checking again
            sleep(Duration::from_millis(100)).await;
        }

        Ok(())
    }

    /// Stop the scheduler immediately without waiting for flows
    pub async fn stop_immediately(&mut self) -> CanoResult<()> {
        *self.running.write().await = false;
        self.command_tx = None;
        Ok(())
    }

    /// Check if any flows are currently running
    pub async fn has_running_flows(&self) -> bool {
        for (_, _, info) in self.flows.values() {
            if info.read().await.status == Status::Running {
                return true;
            }
        }
        false
    }

    /// Get count of currently running flows
    pub async fn running_count(&self) -> usize {
        let mut count = 0;
        for (_, _, info) in self.flows.values() {
            if info.read().await.status == Status::Running {
                count += 1;
            }
        }
        count
    }

    /// Get flow status
    pub async fn status(&self, id: &str) -> Option<FlowInfo> {
        if let Some((_, _, info)) = self.flows.get(id) {
            Some(info.read().await.clone())
        } else {
            None
        }
    }

    /// List all flows
    pub async fn list(&self) -> Vec<FlowInfo> {
        let mut result = Vec::new();
        for (_, _, info) in self.flows.values() {
            result.push(info.read().await.clone());
        }
        result
    }
}

impl<T, S> Default for Stream<T, S>
where
    T: Clone + Send + Sync + 'static + std::fmt::Debug + std::hash::Hash + Eq,
    S: Store + Clone + Default + 'static,
{
    fn default() -> Self {
        Self::new()
    }
}

/// Check if a flow should run
fn should_run(
    schedule: &Schedule,
    last_check: &mut HashMap<String, DateTime<Utc>>,
    id: &str,
    now: DateTime<Utc>,
) -> bool {
    match schedule {
        Schedule::Manual => false,
        Schedule::Every(interval) => {
            let duration =
                chrono::Duration::from_std(*interval).unwrap_or(chrono::Duration::seconds(60));
            if let Some(last) = last_check.get(id) {
                if now >= *last + duration {
                    last_check.insert(id.to_string(), now);
                    true
                } else {
                    false
                }
            } else {
                last_check.insert(id.to_string(), now);
                true
            }
        }
        Schedule::Cron(expr) => {
            if let Ok(schedule) = CronSchedule::from_str(expr) {
                let last = last_check
                    .get(id)
                    .copied()
                    .unwrap_or_else(|| now - chrono::Duration::seconds(1));

                // Check if there's a scheduled time between last check and now
                for upcoming in schedule.after(&last).take(1) {
                    if upcoming <= now {
                        last_check.insert(id.to_string(), now);
                        return true;
                    }
                }
            }
            false
        }
    }
}

/// Execute a flow
async fn execute_flow<T, S>(flow: Arc<Flow<T, S>>, info: Arc<RwLock<FlowInfo>>, store: S)
where
    T: Clone + Send + Sync + 'static + std::fmt::Debug + std::hash::Hash + Eq,
    S: Store + Clone + Default + 'static,
{
    // Update status to running
    {
        let mut info_guard = info.write().await;
        info_guard.status = Status::Running;
        info_guard.last_run = Some(Utc::now());
    }

    // Execute flow
    let result = flow.orchestrate(&store).await;

    // Update final status
    {
        let mut info_guard = info.write().await;
        match result {
            Ok(_) => {
                info_guard.status = Status::Idle;
                info_guard.run_count += 1;
            }
            Err(e) => {
                info_guard.status = Status::Failed(e.to_string());
            }
        }
    }
}
