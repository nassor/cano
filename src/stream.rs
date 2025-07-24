//! # Stream API - Advanced Workflow Scheduling
//!
//! This module provides the [`Stream`] type for scheduling and managing multiple workflows
//! with cron support. The Stream acts as a higher-level scheduler that can manage multiple
//! flows with different execution patterns.
//!
//! ## 🎯 Core Concepts
//!
//! ### Stream as a Scheduler
//!
//! The [`Stream`] provides advanced scheduling capabilities:
//! - Schedule multiple flows with different triggers
//! - Cron-based scheduling for time-based automation
//! - Manual trigger support for event-driven workflows
//! - Concurrent execution of multiple flows
//!
//! ### Flow Management
//!
//! Each stream can manage multiple flows:
//! - Register flows with unique identifiers
//! - Configure different execution patterns per flow
//! - Monitor flow execution status
//! - Handle flow errors and retries
//!
//! ## 🚀 Quick Start
//!
//! Create a stream, register your flows with schedules, and start the scheduler:
//!
//! ```rust
//! use cano::prelude::*;
//! use cano::stream::{Stream, FlowSchedule};
//!
//! #[derive(Debug, Clone, PartialEq, Eq, Hash)]
//! enum MyState {
//!     Start,
//!     End,
//! }
//!
//! #[tokio::main]
//! async fn main() -> CanoResult<()> {
//!     let mut stream: Stream<MyState, MemoryStore> = Stream::new();
//!     
//!     // Create some flows
//!     let flow1 = Flow::new(MyState::Start);
//!     let flow2 = Flow::new(MyState::Start);
//!     
//!     // Add a flow with cron schedule (every hour)
//!     stream.add_flow("hourly_report", flow1, FlowSchedule::Cron("0 0 * * * *".to_string()))?;
//!     
//!     // Add a manually triggered flow
//!     stream.add_flow("manual_task", flow2, FlowSchedule::Manual)?;
//!     
//!     // Start the scheduler
//!     stream.start().await?;
//!     
//!     Ok(())
//! }
//! ```
//!
//! ## 💡 Advanced Features
//!
//! ### Cron Support
//!
//! Full cron expression support for time-based scheduling:
//! - Standard cron format: `sec min hour day month dow year`
//! - Common patterns like `@hourly`, `@daily`, `@weekly`
//! - Custom expressions for complex scheduling needs
//!
//! ### Flow Monitoring
//!
//! Track and monitor flow execution:
//! - Execution history and metrics
//! - Error tracking and alerting
//! - Performance monitoring
//! - Flow status reporting

use crate::error::{CanoError, CanoResult};
use crate::flow::Flow;
use crate::store::Store;
use chrono::{DateTime, Utc};
use cron::Schedule;
use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Arc;
use tokio::sync::{RwLock, mpsc};
use tokio::time::{Duration, sleep};

/// Represents different scheduling modes for flows
#[derive(Debug, Clone)]
pub enum FlowSchedule {
    /// Manual trigger - flow runs only when explicitly triggered
    Manual,
    /// Cron-based scheduling using standard cron expressions
    Cron(String),
    /// Interval-based scheduling (runs every N seconds)
    Interval(u64),
    /// One-time execution at a specific time
    Once(DateTime<Utc>),
}

/// Execution status of a flow
#[derive(Debug, Clone)]
pub enum FlowStatus {
    /// Flow is waiting to be scheduled
    Waiting,
    /// Flow is currently running
    Running,
    /// Flow completed successfully
    Completed,
    /// Flow failed with an error
    Failed(String),
    /// Flow was cancelled
    Cancelled,
}

/// Information about a scheduled flow
#[derive(Debug, Clone)]
pub struct FlowInfo {
    pub id: String,
    pub schedule: FlowSchedule,
    pub status: FlowStatus,
    pub last_run: Option<DateTime<Utc>>,
    pub next_run: Option<DateTime<Utc>>,
    pub run_count: u64,
    pub error_count: u64,
    pub active_instances: u64, // Track how many instances are currently running
}

/// A scheduled flow with its execution context
struct ScheduledFlow<T, S>
where
    T: Clone + Send + Sync + 'static + std::fmt::Debug + std::hash::Hash + Eq,
    S: Store + Clone + Default + 'static,
{
    id: String,
    flow: Arc<Flow<T, S>>,
    schedule: FlowSchedule,
    info: Arc<RwLock<FlowInfo>>,
    store: S,
}

/// Stream scheduler for managing multiple flows
pub struct Stream<T, S>
where
    T: Clone + Send + Sync + 'static + std::fmt::Debug + std::hash::Hash + Eq,
    S: Store + Clone + Default + 'static,
{
    flows: HashMap<String, ScheduledFlow<T, S>>,
    command_sender: Option<mpsc::UnboundedSender<StreamCommand>>,
    running: Arc<RwLock<bool>>,
}

/// Commands for controlling the stream
#[derive(Debug)]
enum StreamCommand {
    TriggerFlow(String),
    StopFlow(String),
    Shutdown,
}

impl<T, S> Stream<T, S>
where
    T: Clone + Send + Sync + 'static + std::fmt::Debug + std::hash::Hash + Eq,
    S: Store + Clone + Default + 'static,
{
    /// Create a new stream scheduler
    pub fn new() -> Self {
        Self {
            flows: HashMap::new(),
            command_sender: None,
            running: Arc::new(RwLock::new(false)),
        }
    }

    /// Add a flow to the stream with a schedule
    pub fn add_flow(
        &mut self,
        id: &str,
        flow: Flow<T, S>,
        schedule: FlowSchedule,
    ) -> CanoResult<()> {
        if self.flows.contains_key(id) {
            return Err(CanoError::Configuration(format!(
                "Flow with id '{id}' already exists",
            )));
        }

        let next_run = match &schedule {
            FlowSchedule::Manual => None,
            FlowSchedule::Cron(expr) => {
                let schedule = Schedule::from_str(expr).map_err(|e| {
                    CanoError::Configuration(format!("Invalid cron expression: {e}"))
                })?;
                schedule.upcoming(Utc).next()
            }
            FlowSchedule::Interval(seconds) => {
                Some(Utc::now() + chrono::Duration::seconds(*seconds as i64))
            }
            FlowSchedule::Once(datetime) => Some(*datetime),
        };

        let flow_info = FlowInfo {
            id: id.to_string(),
            schedule: schedule.clone(),
            status: FlowStatus::Waiting,
            last_run: None,
            next_run,
            run_count: 0,
            error_count: 0,
            active_instances: 0,
        };

        let scheduled_flow = ScheduledFlow {
            id: id.to_string(),
            flow: Arc::new(flow),
            schedule,
            info: Arc::new(RwLock::new(flow_info)),
            store: S::default(),
        };

        self.flows.insert(id.to_string(), scheduled_flow);
        Ok(())
    }

    /// Remove a flow from the stream
    pub fn remove_flow(&mut self, id: &str) -> CanoResult<()> {
        if self.flows.remove(id).is_some() {
            Ok(())
        } else {
            Err(CanoError::Configuration(format!(
                "Flow with id '{id}' not found",
            )))
        }
    }

    /// Get information about a specific flow
    pub async fn get_flow_info(&self, id: &str) -> CanoResult<FlowInfo> {
        if let Some(scheduled_flow) = self.flows.get(id) {
            Ok(scheduled_flow.info.read().await.clone())
        } else {
            Err(CanoError::Configuration(format!(
                "Flow with id '{id}' not found",
            )))
        }
    }

    /// Get information about all flows
    pub async fn get_all_flows_info(&self) -> Vec<FlowInfo> {
        let mut infos = Vec::new();
        for scheduled_flow in self.flows.values() {
            infos.push(scheduled_flow.info.read().await.clone());
        }
        infos
    }

    /// Manually trigger a flow execution
    pub async fn trigger_flow(&self, id: &str) -> CanoResult<()> {
        if let Some(sender) = &self.command_sender {
            sender
                .send(StreamCommand::TriggerFlow(id.to_string()))
                .map_err(|_| CanoError::flow("Failed to send trigger command"))?;
            Ok(())
        } else {
            Err(CanoError::Configuration(
                "Stream is not running".to_string(),
            ))
        }
    }

    /// Stop a specific flow
    pub async fn stop_flow(&self, id: &str) -> CanoResult<()> {
        if let Some(sender) = &self.command_sender {
            sender
                .send(StreamCommand::StopFlow(id.to_string()))
                .map_err(|_| CanoError::flow("Failed to send stop command"))?;
            Ok(())
        } else {
            Err(CanoError::Configuration(
                "Stream is not running".to_string(),
            ))
        }
    }

    /// Start the stream scheduler
    pub async fn start(&mut self) -> CanoResult<()> {
        if *self.running.read().await {
            return Err(CanoError::Configuration(
                "Stream is already running".to_string(),
            ));
        }

        let (command_sender, mut command_receiver) = mpsc::unbounded_channel();
        self.command_sender = Some(command_sender);

        *self.running.write().await = true;
        let running = Arc::clone(&self.running);

        // Move flows out of self for the scheduler task
        let mut flows = std::mem::take(&mut self.flows);

        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(1));

            loop {
                tokio::select! {
                    _ = interval.tick() => {
                        if !*running.read().await {
                            break;
                        }

                        let now = Utc::now();
                        for (_, scheduled_flow) in flows.iter_mut() {
                            if should_run_flow(&scheduled_flow.schedule, &scheduled_flow.info, now).await {
                                let flow_id = scheduled_flow.id.clone();
                                let flow = Arc::clone(&scheduled_flow.flow);
                                let store = scheduled_flow.store.clone();
                                let info = Arc::clone(&scheduled_flow.info);

                                tokio::spawn(async move {
                                    Self::execute_flow(flow_id, flow, store, info).await;
                                });
                            }
                        }
                    }

                    command = command_receiver.recv() => {
                        match command {
                            Some(StreamCommand::TriggerFlow(id)) => {
                                if let Some(scheduled_flow) = flows.get_mut(&id) {
                                    let flow_id = scheduled_flow.id.clone();
                                    let flow = Arc::clone(&scheduled_flow.flow);
                                    let store = scheduled_flow.store.clone();
                                    let info = Arc::clone(&scheduled_flow.info);

                                    tokio::spawn(async move {
                                        Self::execute_flow(flow_id, flow, store, info).await;
                                    });
                                }
                            }
                            Some(StreamCommand::StopFlow(id)) => {
                                if let Some(scheduled_flow) = flows.get_mut(&id) {
                                    let mut info = scheduled_flow.info.write().await;
                                    info.status = FlowStatus::Cancelled;
                                }
                            }
                            Some(StreamCommand::Shutdown) | None => {
                                break;
                            }
                        }
                    }
                }
            }

            *running.write().await = false;
        });

        Ok(())
    }

    /// Stop the stream scheduler
    pub async fn stop(&self) -> CanoResult<()> {
        if let Some(sender) = &self.command_sender {
            sender
                .send(StreamCommand::Shutdown)
                .map_err(|_| CanoError::flow("Failed to send shutdown command"))?;
        }

        // Wait for the scheduler to stop
        while *self.running.read().await {
            sleep(Duration::from_millis(10)).await;
        }

        Ok(())
    }

    /// Check if the stream is currently running
    pub async fn is_running(&self) -> bool {
        *self.running.read().await
    }

    /// Execute a flow and update its status
    async fn execute_flow(
        _flow_id: String,
        flow: Arc<Flow<T, S>>,
        store: S,
        info: Arc<RwLock<FlowInfo>>,
    ) {
        // Increment active instance count and update status
        {
            let mut info_guard = info.write().await;
            info_guard.active_instances += 1;
            info_guard.status = FlowStatus::Running;
            info_guard.last_run = Some(Utc::now());
        }

        // Execute the flow
        let result = flow.orchestrate(&store).await;

        // Update status based on result and decrement active instances
        {
            let mut info_guard = info.write().await;
            info_guard.active_instances -= 1;

            match result {
                Ok(_) => {
                    info_guard.run_count += 1;
                    // Only update status to Completed if no other instances are running
                    if info_guard.active_instances == 0 {
                        info_guard.status = FlowStatus::Completed;
                    }
                }
                Err(e) => {
                    info_guard.error_count += 1;
                    // Only update status to Failed if no other instances are running
                    if info_guard.active_instances == 0 {
                        info_guard.status = FlowStatus::Failed(e.to_string());
                    }
                }
            }

            // Calculate next run time only if no instances are running
            // This prevents race conditions when multiple instances complete simultaneously
            if info_guard.active_instances == 0 {
                info_guard.next_run = calculate_next_run(&info_guard.schedule);
            }
        }
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

/// Check if a flow should run based on its schedule
async fn should_run_flow(
    schedule: &FlowSchedule,
    info: &Arc<RwLock<FlowInfo>>,
    now: DateTime<Utc>,
) -> bool {
    let info_guard = info.read().await;

    // Allow concurrent executions - remove the "already running" check
    // This enables the same flow to run multiple times simultaneously

    match schedule {
        FlowSchedule::Manual => false, // Only run when manually triggered
        FlowSchedule::Cron(_) | FlowSchedule::Interval(_) | FlowSchedule::Once(_) => {
            if let Some(next_run) = info_guard.next_run {
                now >= next_run
            } else {
                false
            }
        }
    }
}

/// Calculate the next run time for a schedule
fn calculate_next_run(schedule: &FlowSchedule) -> Option<DateTime<Utc>> {
    match schedule {
        FlowSchedule::Manual => None,
        FlowSchedule::Cron(expr) => {
            if let Ok(schedule) = Schedule::from_str(expr) {
                schedule.upcoming(Utc).next()
            } else {
                None
            }
        }
        FlowSchedule::Interval(seconds) => {
            Some(Utc::now() + chrono::Duration::seconds(*seconds as i64))
        }
        FlowSchedule::Once(_) => None, // One-time execution, no next run
    }
}

/// Builder for creating streams with fluent API
pub struct StreamBuilder<T, S>
where
    T: Clone + Send + Sync + 'static + std::fmt::Debug + std::hash::Hash + Eq,
    S: Store + Clone + Default + 'static,
{
    stream: Stream<T, S>,
}

impl<T, S> StreamBuilder<T, S>
where
    T: Clone + Send + Sync + 'static + std::fmt::Debug + std::hash::Hash + Eq,
    S: Store + Clone + Default + 'static,
{
    /// Create a new stream builder
    pub fn new() -> Self {
        Self {
            stream: Stream::new(),
        }
    }

    /// Add a flow with cron schedule
    pub fn with_cron_flow(
        mut self,
        id: &str,
        flow: Flow<T, S>,
        cron_expr: &str,
    ) -> CanoResult<Self> {
        self.stream
            .add_flow(id, flow, FlowSchedule::Cron(cron_expr.to_string()))?;
        Ok(self)
    }

    /// Add a flow with interval schedule
    pub fn with_interval_flow(
        mut self,
        id: &str,
        flow: Flow<T, S>,
        interval_seconds: u64,
    ) -> CanoResult<Self> {
        self.stream
            .add_flow(id, flow, FlowSchedule::Interval(interval_seconds))?;
        Ok(self)
    }

    /// Add a manually triggered flow
    pub fn with_manual_flow(mut self, id: &str, flow: Flow<T, S>) -> CanoResult<Self> {
        self.stream.add_flow(id, flow, FlowSchedule::Manual)?;
        Ok(self)
    }

    /// Add a one-time flow
    pub fn with_once_flow(
        mut self,
        id: &str,
        flow: Flow<T, S>,
        run_at: DateTime<Utc>,
    ) -> CanoResult<Self> {
        self.stream.add_flow(id, flow, FlowSchedule::Once(run_at))?;
        Ok(self)
    }

    /// Build the stream
    pub fn build(self) -> Stream<T, S> {
        self.stream
    }
}

impl<T, S> Default for StreamBuilder<T, S>
where
    T: Clone + Send + Sync + 'static + std::fmt::Debug + std::hash::Hash + Eq,
    S: Store + Clone + Default + 'static,
{
    fn default() -> Self {
        Self::new()
    }
}
