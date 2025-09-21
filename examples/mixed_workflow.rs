//! # Mixed Task and Node Workflow Example
//!
//! This example demonstrates the power of Cano's unified registration system by
//! mixing both Tasks and Nodes in the same workflow:
//!
//! 1. **DataGeneratorTask**: Simple task that generates test data
//! 2. **ProcessorNode**: Structured node with retry logic for data processing
//! 3. **ValidatorTask**: Quick validation task
//! 4. **ReportNode**: Structured node for generating final reports
//!
//! **Key Features Demonstrated:**
//! - **Unified Registration**: Both Tasks and Nodes use `.register()` method
//! - **Seamless Interoperability**: Tasks and Nodes work together in one workflow
//! - **Flexible Architecture**: Choose Task or Node based on specific needs
//! - **Automatic Compatibility**: Every Node implements Task automatically
//!
//! **When to use Task vs Node:**
//! - Use **Task** for simple, flexible processing where you want full control
//! - Use **Node** for structured processing with built-in retry and error handling
//!
//! Run with:
//! ```bash
//! cargo run --example mixed_workflow
//! ```

use async_trait::async_trait;
use cano::prelude::*;
use rand::Rng;

/// Workflow states
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum WorkflowState {
    GenerateData,
    ProcessData,
    ValidateResults,
    GenerateReport,
    Complete,
}

/// Simple Task for data generation - maximum flexibility
struct DataGeneratorTask {
    size: usize,
}

impl DataGeneratorTask {
    fn new(size: usize) -> Self {
        Self { size }
    }
}

#[async_trait]
impl Task<WorkflowState> for DataGeneratorTask {
    async fn run(&self, store: &MemoryStore) -> CanoResult<WorkflowState> {
        println!(
            "📊 DataGeneratorTask: Generating {} data points...",
            self.size
        );

        let mut rng = rand::rng();
        let data: Vec<f64> = (0..self.size)
            .map(|_| rng.random_range(0.0..100.0))
            .collect();

        println!(
            "   Generated data range: {:.2} to {:.2}",
            data.iter().fold(f64::INFINITY, |a, &b| a.min(b)),
            data.iter().fold(f64::NEG_INFINITY, |a, &b| a.max(b))
        );

        store.put("raw_data", data)?;
        store.put("generation_time", std::time::SystemTime::now())?;

        Ok(WorkflowState::ProcessData)
    }
}

/// Structured Node for data processing - built-in retry and error handling
struct ProcessorNode {
    threshold: f64,
}

impl ProcessorNode {
    fn new(threshold: f64) -> Self {
        Self { threshold }
    }
}

#[async_trait]
impl Node<WorkflowState> for ProcessorNode {
    type PrepResult = Vec<f64>;
    type ExecResult = (Vec<f64>, DataStats);

    async fn prep(&self, store: &MemoryStore) -> CanoResult<Self::PrepResult> {
        println!("🔧 ProcessorNode::prep - Loading and validating data...");

        let data: Vec<f64> = store.get("raw_data")?;
        if data.is_empty() {
            return Err(CanoError::node_execution("No data to process".to_string()));
        }

        println!("   Loaded {} data points for processing", data.len());
        Ok(data)
    }

    async fn exec(&self, raw_data: Self::PrepResult) -> Self::ExecResult {
        println!(
            "⚙️  ProcessorNode::exec - Processing data with threshold {}...",
            self.threshold
        );

        // Complex processing that might benefit from retry logic
        let processed_data: Vec<f64> = raw_data
            .iter()
            .filter(|&&x| x > self.threshold)
            .map(|&x| x * 1.5) // Apply some transformation
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
        (processed_data, stats)
    }

    async fn post(
        &self,
        store: &MemoryStore,
        exec_res: Self::ExecResult,
    ) -> CanoResult<WorkflowState> {
        println!("📋 ProcessorNode::post - Finalizing processing...");

        let (processed_data, stats) = exec_res;

        store.put("processed_data", processed_data)?;
        store.put("stats", stats.clone())?;

        if stats.count == 0 {
            println!("   ⚠️  No data survived processing - might need to adjust threshold");
            return Ok(WorkflowState::GenerateReport); // Skip validation
        }

        println!(
            "   ✅ Processing complete - {} points ready for validation",
            stats.count
        );
        Ok(WorkflowState::ValidateResults)
    }
}

/// Simple Task for quick validation
struct ValidatorTask;

#[async_trait]
impl Task<WorkflowState> for ValidatorTask {
    async fn run(&self, store: &MemoryStore) -> CanoResult<WorkflowState> {
        println!("✅ ValidatorTask: Running validation checks...");

        let stats: DataStats = store.get("stats")?;
        let processed_data: Vec<f64> = store.get("processed_data")?;

        // Quick validation checks
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
            println!("   ✅ All validation checks passed!");
        } else {
            println!(
                "   ⚠️  Found {} validation issues",
                validation_results.len()
            );
            for error in &validation_results {
                println!("      - {error}");
            }
        }

        Ok(WorkflowState::GenerateReport)
    }
}

/// Structured Node for report generation
struct ReportNode;

#[async_trait]
impl Node<WorkflowState> for ReportNode {
    type PrepResult = ();
    type ExecResult = ();

    async fn prep(&self, store: &MemoryStore) -> CanoResult<Self::PrepResult> {
        println!("📊 ReportNode::prep - Gathering report data...");

        // Ensure we have all required data
        let _stats: DataStats = store.get("stats")?;
        let _validation_errors: Vec<String> = store.get("validation_errors")?;

        Ok(())
    }

    async fn exec(&self, _prep_res: Self::PrepResult) -> Self::ExecResult {
        println!("📝 ReportNode::exec - Generating comprehensive report...");

        // Simulate report generation
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
    }

    async fn post(
        &self,
        store: &MemoryStore,
        _exec_res: Self::ExecResult,
    ) -> CanoResult<WorkflowState> {
        println!("📋 ReportNode::post - Finalizing report...");

        let stats: DataStats = store.get("stats")?;
        let validation_errors: Vec<String> = store.get("validation_errors")?;

        // Generate final report
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
                "✅ PASSED"
            } else {
                "⚠️  WITH WARNINGS"
            }
        );

        store.put("final_report", report)?;
        println!("   📄 Report generated successfully!");

        Ok(WorkflowState::Complete)
    }
}

#[derive(Debug, Clone)]
struct DataStats {
    count: usize,
    mean: f64,
    min: f64,
    max: f64,
}

#[tokio::main]
async fn main() -> CanoResult<()> {
    println!("🚀 Starting Mixed Task/Node workflow example\n");

    // Create workflow
    let mut workflow = Workflow::new(WorkflowState::GenerateData);

    // Register mix of Tasks and Nodes using the unified .register() method
    println!("🔧 Registering workflow components:");
    println!("   📊 DataGeneratorTask (Task) -> Generate");
    workflow.register(WorkflowState::GenerateData, DataGeneratorTask::new(20));

    println!("   ⚙️  ProcessorNode (Node) -> Process");
    workflow.register(WorkflowState::ProcessData, ProcessorNode::new(25.0));

    println!("   ✅ ValidatorTask (Task) -> Validate");
    workflow.register(WorkflowState::ValidateResults, ValidatorTask);

    println!("   📊 ReportNode (Node) -> Report");
    workflow.register(WorkflowState::GenerateReport, ReportNode);

    // Set exit state
    workflow.add_exit_states(vec![WorkflowState::Complete]);

    println!("\n🎯 Running mixed Task/Node workflow...\n");

    // Run the workflow
    let store = MemoryStore::new();
    match workflow.orchestrate(&store).await {
        Ok(_final_state) => {
            println!("\n🎉 Mixed workflow completed successfully!");

            if let Ok(report) = store.get::<String>("final_report") {
                println!("\n{report}");
            }
        }
        Err(e) => {
            eprintln!("❌ Workflow failed: {e}");
            return Err(e);
        }
    }

    println!("\n💡 This example shows how Tasks and Nodes work together:");
    println!("   • Tasks provide flexibility for simple operations");
    println!("   • Nodes provide structure for complex processing with retry logic");
    println!("   • Both use the same .register() method");
    println!("   • Both can be mixed freely in the same workflow");

    Ok(())
}
