//! # Multi-Task Workflow Example
//!
//! This example demonstrates a four-task workflow using `Task` for every step:
//!
//! 1. **DataGeneratorTask**: Generates test data
//! 2. **ProcessorTask**: Processes data above a threshold
//! 3. **ValidatorTask**: Validates processed results
//! 4. **ReportTask**: Generates a final report
//!
//! Run with:
//! ```bash
//! cargo run --example mixed_workflow
//! ```

use cano::prelude::*;
use rand::RngExt;

/// Workflow states
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum WorkflowState {
    GenerateData,
    ProcessData,
    ValidateResults,
    GenerateReport,
    Complete,
}

/// Simple Task for data generation
struct DataGeneratorTask {
    size: usize,
}

impl DataGeneratorTask {
    fn new(size: usize) -> Self {
        Self { size }
    }
}

#[task(state = WorkflowState)]
impl DataGeneratorTask {
    async fn run(&self, res: &Resources) -> CanoResult<TaskResult<WorkflowState>> {
        let store = res.get::<MemoryStore, _>("store")?;

        println!("DataGeneratorTask: Generating {} data points...", self.size);

        let mut rng = rand::rng();
        let data: Vec<f64> = (0..self.size)
            .map(|_| rng.random_range(0.0..100.0))
            .collect();

        println!(
            "   Generated data range: {:.2} to {:.2}",
            data.iter().fold(f64::INFINITY, |a: f64, &b| a.min(b)),
            data.iter().fold(f64::NEG_INFINITY, |a: f64, &b| a.max(b))
        );

        store.put("raw_data", data)?;
        store.put("generation_time", std::time::SystemTime::now())?;

        Ok(TaskResult::Single(WorkflowState::ProcessData))
    }
}

#[derive(Debug, Clone)]
struct DataStats {
    count: usize,
    mean: f64,
    min: f64,
    max: f64,
}

/// Task for data processing with a threshold filter
struct ProcessorTask {
    threshold: f64,
}

impl ProcessorTask {
    fn new(threshold: f64) -> Self {
        Self { threshold }
    }
}

#[task(state = WorkflowState)]
impl ProcessorTask {
    async fn run(&self, res: &Resources) -> CanoResult<TaskResult<WorkflowState>> {
        let store = res.get::<MemoryStore, _>("store")?;

        println!(
            "ProcessorTask: Processing data with threshold {}...",
            self.threshold
        );

        let data: Vec<f64> = store.get("raw_data")?;
        if data.is_empty() {
            return Err(CanoError::task_execution("No data to process".to_string()));
        }

        println!("   Loaded {} data points", data.len());

        let processed_data: Vec<f64> = data
            .iter()
            .filter(|&&x| x > self.threshold)
            .map(|&x| x * 1.5)
            .collect();

        let stats = if processed_data.is_empty() {
            DataStats {
                count: 0,
                mean: 0.0,
                max: 0.0,
                min: 0.0,
            }
        } else {
            DataStats {
                count: processed_data.len(),
                mean: processed_data.iter().sum::<f64>() / processed_data.len() as f64,
                max: processed_data
                    .iter()
                    .fold(f64::NEG_INFINITY, |a, &b| a.max(b)),
                min: processed_data.iter().fold(f64::INFINITY, |a, &b| a.min(b)),
            }
        };

        println!("   Processed {} points above threshold", stats.count);

        store.put("processed_data", processed_data)?;
        store.put("stats", stats.clone())?;

        if stats.count == 0 {
            println!("   No data survived processing — skipping validation");
            return Ok(TaskResult::Single(WorkflowState::GenerateReport));
        }

        println!(
            "   Processing complete — {} points ready for validation",
            stats.count
        );
        Ok(TaskResult::Single(WorkflowState::ValidateResults))
    }
}

/// Task for quick validation
struct ValidatorTask;

#[task(state = WorkflowState)]
impl ValidatorTask {
    async fn run(&self, res: &Resources) -> CanoResult<TaskResult<WorkflowState>> {
        let store = res.get::<MemoryStore, _>("store")?;

        println!("ValidatorTask: Running validation checks...");

        let stats: DataStats = store.get("stats")?;
        let processed_data: Vec<f64> = store.get("processed_data")?;

        let mut validation_results = Vec::new();

        if stats.count != processed_data.len() {
            validation_results.push("Count mismatch between stats and data".to_string());
        }

        if stats.mean.is_nan() || stats.mean.is_infinite() {
            validation_results.push("Invalid mean value".to_string());
        }

        if processed_data
            .iter()
            .any(|&x| x.is_nan() || x.is_infinite())
        {
            validation_results.push("Invalid values in processed data".to_string());
        }

        store.put("validation_errors", validation_results.clone())?;

        if validation_results.is_empty() {
            println!("   All validation checks passed!");
        } else {
            println!("   Found {} validation issues", validation_results.len());
            for error in &validation_results {
                println!("      - {error}");
            }
        }

        Ok(TaskResult::Single(WorkflowState::GenerateReport))
    }
}

/// Task for report generation
struct ReportTask;

#[task(state = WorkflowState)]
impl ReportTask {
    async fn run(&self, res: &Resources) -> CanoResult<TaskResult<WorkflowState>> {
        let store = res.get::<MemoryStore, _>("store")?;

        println!("ReportTask: Generating report...");

        let stats: DataStats = store.get("stats")?;
        let validation_errors: Vec<String> = store.get("validation_errors")?;

        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

        let report = format!(
            "=== PROCESSING REPORT ===\n\
             Data Points Processed: {}\n\
             Mean Value: {:.2}\n\
             Min Value: {:.2}\n\
             Max Value: {:.2}\n\
             Validation Issues: {}\n\
             Status: {}",
            stats.count,
            stats.mean,
            stats.min,
            stats.max,
            validation_errors.len(),
            if validation_errors.is_empty() {
                "PASSED"
            } else {
                "WITH WARNINGS"
            }
        );

        store.put("final_report", report)?;
        println!("   Report generated successfully!");

        Ok(TaskResult::Single(WorkflowState::Complete))
    }
}

#[tokio::main]
async fn main() -> CanoResult<()> {
    println!("Starting multi-task workflow example\n");

    let store = MemoryStore::new();
    let resources = Resources::new().insert("store", store.clone());

    let workflow = Workflow::new(resources)
        .register(WorkflowState::GenerateData, DataGeneratorTask::new(20))
        .register(WorkflowState::ProcessData, ProcessorTask::new(25.0))
        .register(WorkflowState::ValidateResults, ValidatorTask)
        .register(WorkflowState::GenerateReport, ReportTask)
        .add_exit_states(vec![WorkflowState::Complete]);

    match workflow.orchestrate(WorkflowState::GenerateData).await {
        Ok(_final_state) => {
            println!("\nWorkflow completed successfully!");

            if let Ok(report) = store.get::<String>("final_report") {
                println!("\n{report}");
            }
        }
        Err(e) => {
            eprintln!("Workflow failed: {e}");
            return Err(e);
        }
    }

    Ok(())
}
