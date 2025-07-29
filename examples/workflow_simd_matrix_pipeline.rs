//! # SIMD Matrix Processing Pipeline Example
//!
//! This example demonstrates a sophisticated data processing pipeline using the `wide` crate
//! for SIMD (Single Instruction, Multiple Data) acceleration in a Cano workflow.
//!
//! ## Pipeline Architecture
//!
//! The pipeline consists of four stages, each implementing different SIMD-accelerated operations:
//!
//! 1. **Data Generation** (`MatrixGenerator`): Creates random 64x64 matrices filled with float32 values
//! 2. **Matrix Multiplication** (`SimdMatrixMultiplier`): Performs matrix multiplication using SIMD f32x8 vectors
//! 3. **Matrix Transformation** (`SimdMatrixTransformer`): Applies scaling and element-wise addition using SIMD
//! 4. **Statistical Analysis** (`SimdStatisticsCalculator`): Computes sum, mean, and variance using SIMD operations
//!
//! ## SIMD Optimizations Demonstrated
//!
//! - **f32x8 Vector Operations**: Processes 8 float32 values simultaneously
//! - **Matrix Multiplication**: Optimized inner loops using SIMD for better cache efficiency
//! - **Element-wise Operations**: Vectorized addition, scaling, and statistical calculations
//! - **Memory Access Patterns**: Aligned memory access for optimal SIMD performance
//!
//! ## Key Features
//!
//! - Uses Cano's three-phase node execution (prep, exec, post)
//! - Inter-node communication through shared MemoryStore
//! - State-driven workflow orchestration with typed transitions
//! - Performance monitoring with execution timing
//! - Graceful handling of partial SIMD vectors (when data size isn't divisible by 8)
//!
//! ## Performance Benefits
//!
//! The SIMD operations provide significant performance improvements over scalar operations:
//! - Matrix multiplication: ~8x theoretical speedup for float32 operations
//! - Element-wise operations: Nearly linear scaling with vector width
//! - Statistical calculations: Reduced loop iterations and improved cache utilization
//!
//! ## Usage
//!
//! ```bash
//! cargo run --example simd_matrix_pipeline
//! ```
//!
//! The example generates 20 matrices of size 64x64, processes them through the pipeline,
//! and reports timing information for each stage.

use cano::prelude::*;
use std::time::Instant;
use wide::f32x8;

/// Matrix structure optimized for SIMD operations
#[derive(Debug, Clone)]
struct SimdMatrix {
    data: Vec<f32>,
    rows: usize,
    cols: usize,
}

impl SimdMatrix {
    fn new(rows: usize, cols: usize) -> Self {
        Self {
            data: vec![0.0; rows * cols],
            rows,
            cols,
        }
    }

    fn from_data(data: Vec<f32>, rows: usize, cols: usize) -> Self {
        assert_eq!(data.len(), rows * cols);
        Self { data, rows, cols }
    }

    fn get(&self, row: usize, col: usize) -> f32 {
        self.data[row * self.cols + col]
    }

    fn set(&mut self, row: usize, col: usize, value: f32) {
        self.data[row * self.cols + col] = value;
    }

    /// Standard scalar matrix multiplication for comparison
    fn multiply_scalar(&self, other: &SimdMatrix) -> SimdMatrix {
        assert_eq!(self.cols, other.rows);
        
        let mut result = SimdMatrix::new(self.rows, other.cols);
        
        for i in 0..self.rows {
            for j in 0..other.cols {
                let mut sum = 0.0;
                for k in 0..self.cols {
                    sum += self.get(i, k) * other.get(k, j);
                }
                result.set(i, j, sum);
            }
        }
        
        result
    }

    /// SIMD-accelerated matrix multiplication
    fn multiply_simd(&self, other: &SimdMatrix) -> SimdMatrix {
        assert_eq!(self.cols, other.rows);
        
        let mut result = SimdMatrix::new(self.rows, other.cols);
        
        // Process 8 elements at a time using SIMD
        for i in 0..self.rows {
            for j in (0..other.cols).step_by(8) {
                let mut sum = f32x8::ZERO;
                
                for k in 0..self.cols {
                    let a_val = f32x8::splat(self.get(i, k));
                    
                    // Load 8 consecutive elements from matrix B
                    let remaining = (other.cols - j).min(8);
                    let mut b_vals = [0.0f32; 8];
                    for l in 0..remaining {
                        if j + l < other.cols {
                            b_vals[l] = other.get(k, j + l);
                        }
                    }
                    let b_vec = f32x8::from(b_vals);
                    
                    sum += a_val * b_vec;
                }
                
                // Store the results back
                let sum_array: [f32; 8] = sum.into();
                for l in 0..8 {
                    if j + l < other.cols {
                        result.set(i, j + l, sum_array[l]);
                    }
                }
            }
        }
        
        result
    }

    /// SIMD-accelerated element-wise operations
    fn add_simd(&self, other: &SimdMatrix) -> SimdMatrix {
        assert_eq!(self.rows, other.rows);
        assert_eq!(self.cols, other.cols);
        
        let mut result = SimdMatrix::new(self.rows, self.cols);
        
        // Process 8 elements at a time
        for i in (0..self.data.len()).step_by(8) {
            let remaining = (self.data.len() - i).min(8);
            let mut a_vals = [0.0f32; 8];
            let mut b_vals = [0.0f32; 8];
            
            for j in 0..remaining {
                a_vals[j] = self.data[i + j];
                b_vals[j] = other.data[i + j];
            }
            
            let a_vec = f32x8::from(a_vals);
            let b_vec = f32x8::from(b_vals);
            let sum_vec = a_vec + b_vec;
            let sum_array: [f32; 8] = sum_vec.into();
            
            for j in 0..remaining {
                result.data[i + j] = sum_array[j];
            }
        }
        
        result
    }

    /// SIMD-accelerated scalar multiplication
    fn scale_simd(&self, scalar: f32) -> SimdMatrix {
        let mut result = SimdMatrix::new(self.rows, self.cols);
        let scalar_vec = f32x8::splat(scalar);
        
        for i in (0..self.data.len()).step_by(8) {
            let remaining = (self.data.len() - i).min(8);
            let mut vals = [0.0f32; 8];
            
            for j in 0..remaining {
                vals[j] = self.data[i + j];
            }
            
            let vec = f32x8::from(vals);
            let scaled = vec * scalar_vec;
            let scaled_array: [f32; 8] = scaled.into();
            
            for j in 0..remaining {
                result.data[i + j] = scaled_array[j];
            }
        }
        
        result
    }
}

/// Pipeline states for the SIMD matrix processing workflow
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum PipelineState {
    Generate,
    Multiply,
    Transform,
    Statistics,
    Complete,
    Error,
}

/// Data generation node - creates random matrices
#[derive(Clone)]
struct MatrixGenerator {
    size: usize,
    count: usize,
}

impl MatrixGenerator {
    fn new(size: usize, count: usize) -> Self {
        Self { size, count }
    }
}

#[async_trait::async_trait]
impl Node<PipelineState> for MatrixGenerator {
    type PrepResult = ();
    type ExecResult = Vec<SimdMatrix>;

    fn config(&self) -> NodeConfig {
        NodeConfig::minimal()
    }

    async fn prep(&self, _store: &MemoryStore) -> Result<Self::PrepResult, CanoError> {
        println!("🔢 Preparing to generate {} {}x{} matrices...", self.count, self.size, self.size);
        Ok(())
    }

    async fn exec(&self, _prep_res: Self::PrepResult) -> Self::ExecResult {
        println!("🔢 Generating matrices...");
        
        let mut matrices = Vec::new();
        
        for i in 0..self.count {
            let mut data = Vec::with_capacity(self.size * self.size);
            
            // Generate random matrix data
            for _ in 0..(self.size * self.size) {
                data.push(rand::random::<f32>() * 10.0);
            }
            
            let matrix = SimdMatrix::from_data(data, self.size, self.size);
            matrices.push(matrix);
            
            if i % 10 == 0 {
                println!("  Generated matrix {}/{}", i + 1, self.count);
            }
        }
        
        matrices
    }

    async fn post(&self, store: &MemoryStore, exec_res: Self::ExecResult) -> Result<PipelineState, CanoError> {
        store.put("matrices", exec_res)?;
        println!("✅ Matrix generation complete!");
        Ok(PipelineState::Multiply)
    }
}

/// Matrix multiplication node using SIMD
#[derive(Clone)]
struct SimdMatrixMultiplier;

#[async_trait::async_trait]
impl Node<PipelineState> for SimdMatrixMultiplier {
    type PrepResult = Vec<SimdMatrix>;
    type ExecResult = Vec<SimdMatrix>;

    fn config(&self) -> NodeConfig {
        NodeConfig::minimal()
    }

    async fn prep(&self, store: &MemoryStore) -> Result<Self::PrepResult, CanoError> {
        println!("🧮 Loading matrices for SIMD multiplication...");
        let matrices: Vec<SimdMatrix> = store.get("matrices")?;
        Ok(matrices)
    }

    async fn exec(&self, prep_res: Self::PrepResult) -> Self::ExecResult {
        println!("🧮 Performing SIMD matrix multiplications...");
        let start = Instant::now();
        
        let mut results = Vec::new();
        
        // Multiply consecutive pairs of matrices
        for chunk in prep_res.chunks(2) {
            if chunk.len() == 2 {
                let result = chunk[0].multiply_simd(&chunk[1]);
                results.push(result);
            }
        }
        
        let duration = start.elapsed();
        println!("✅ Matrix multiplications complete in {:?} (SIMD accelerated)", duration);
        println!("  Processed {} matrix pairs", results.len());
        
        results
    }

    async fn post(&self, store: &MemoryStore, exec_res: Self::ExecResult) -> Result<PipelineState, CanoError> {
        store.put("multiplied_matrices", exec_res)?;
        Ok(PipelineState::Transform)
    }
}

/// Matrix transformation node - applies scaling and addition using SIMD
#[derive(Clone)]
struct SimdMatrixTransformer {
    scale_factor: f32,
}

impl SimdMatrixTransformer {
    fn new(scale_factor: f32) -> Self {
        Self { scale_factor }
    }
}

#[async_trait::async_trait]
impl Node<PipelineState> for SimdMatrixTransformer {
    type PrepResult = Vec<SimdMatrix>;
    type ExecResult = Vec<SimdMatrix>;

    fn config(&self) -> NodeConfig {
        NodeConfig::minimal()
    }

    async fn prep(&self, store: &MemoryStore) -> Result<Self::PrepResult, CanoError> {
        println!("🔄 Loading matrices for SIMD transformations...");
        let matrices: Vec<SimdMatrix> = store.get("multiplied_matrices")?;
        Ok(matrices)
    }

    async fn exec(&self, prep_res: Self::PrepResult) -> Self::ExecResult {
        println!("🔄 Applying SIMD transformations (scale factor: {})...", self.scale_factor);
        let start = Instant::now();
        
        let mut results: Vec<SimdMatrix> = Vec::new();
        
        for (i, matrix) in prep_res.iter().enumerate() {
            // Scale the matrix using SIMD
            let scaled = matrix.scale_simd(self.scale_factor);
            
            // If we have multiple matrices, add them together using SIMD
            if i > 0 && !results.is_empty() {
                let last_idx = results.len() - 1;
                let combined = results[last_idx].add_simd(&scaled);
                results[last_idx] = combined;
            } else {
                results.push(scaled);
            }
        }
        
        let duration = start.elapsed();
        println!("✅ Matrix transformations complete in {:?} (SIMD accelerated)", duration);
        println!("  Processed {} matrices", prep_res.len());
        
        results
    }

    async fn post(&self, store: &MemoryStore, exec_res: Self::ExecResult) -> Result<PipelineState, CanoError> {
        store.put("transformed_matrices", exec_res)?;
        Ok(PipelineState::Statistics)
    }
}

/// Statistical analysis node using SIMD for vector operations
#[derive(Clone)]
struct SimdStatisticsCalculator;

#[async_trait::async_trait]
impl Node<PipelineState> for SimdStatisticsCalculator {
    type PrepResult = Vec<SimdMatrix>;
    type ExecResult = Vec<(f32, f32, f32)>; // (sum, mean, variance)

    fn config(&self) -> NodeConfig {
        NodeConfig::minimal()
    }

    async fn prep(&self, store: &MemoryStore) -> Result<Self::PrepResult, CanoError> {
        println!("📊 Loading matrices for statistics calculation...");
        let matrices: Vec<SimdMatrix> = store.get("transformed_matrices")?;
        Ok(matrices)
    }

    async fn exec(&self, prep_res: Self::PrepResult) -> Self::ExecResult {
        println!("📊 Calculating matrix statistics using SIMD...");
        let start = Instant::now();
        
        let mut results = Vec::new();
        
        for matrix in prep_res {
            // Calculate sum using SIMD
            let mut sum = f32x8::ZERO;
            let data_len = matrix.data.len();
            
            for i in (0..data_len).step_by(8) {
                let remaining = (data_len - i).min(8);
                let mut vals = [0.0f32; 8];
                
                for j in 0..remaining {
                    vals[j] = matrix.data[i + j];
                }
                
                let vec = f32x8::from(vals);
                sum += vec;
            }
            
            // Sum all elements in the SIMD vector
            let sum_array: [f32; 8] = sum.into();
            let total_sum = sum_array.iter().sum::<f32>();
            let mean = total_sum / data_len as f32;
            
            // Calculate variance using SIMD
            let mean_vec = f32x8::splat(mean);
            let mut variance_sum = f32x8::ZERO;
            
            for i in (0..data_len).step_by(8) {
                let remaining = (data_len - i).min(8);
                let mut vals = [0.0f32; 8];
                
                for j in 0..remaining {
                    vals[j] = matrix.data[i + j];
                }
                
                let vec = f32x8::from(vals);
                let diff = vec - mean_vec;
                variance_sum += diff * diff;
            }
            
            let variance_array: [f32; 8] = variance_sum.into();
            let total_variance = variance_array.iter().sum::<f32>() / data_len as f32;
            
            results.push((total_sum, mean, total_variance));
        }
        
        let duration = start.elapsed();
        println!("✅ Statistics calculation complete in {:?} (SIMD accelerated)", duration);
        
        results
    }

    async fn post(&self, store: &MemoryStore, exec_res: Self::ExecResult) -> Result<PipelineState, CanoError> {
        store.put("statistics", exec_res.clone())?;
        
        for (i, (sum, mean, variance)) in exec_res.iter().enumerate() {
            println!("  Matrix {}: sum={:.2}, mean={:.2}, variance={:.2}", 
                     i + 1, sum, mean, variance);
        }
        
        Ok(PipelineState::Complete)
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("🚀 Starting SIMD Matrix Processing Pipeline");
    println!("This example demonstrates SIMD-accelerated mathematical operations");
    println!("using the 'wide' crate in a Cano workflow pipeline.\n");

    // Quick SIMD vs Scalar performance comparison
    println!("🏁 Quick Performance Comparison (SIMD vs Scalar):");
    let test_matrix_a = {
        let mut data = Vec::with_capacity(16 * 16);
        for _ in 0..(16 * 16) {
            data.push(rand::random::<f32>() * 10.0);
        }
        SimdMatrix::from_data(data, 16, 16)
    };
    let test_matrix_b = {
        let mut data = Vec::with_capacity(16 * 16);
        for _ in 0..(16 * 16) {
            data.push(rand::random::<f32>() * 10.0);
        }
        SimdMatrix::from_data(data, 16, 16)
    };

    // Scalar multiplication timing
    let scalar_start = Instant::now();
    let _scalar_result = test_matrix_a.multiply_scalar(&test_matrix_b);
    let scalar_duration = scalar_start.elapsed();

    // SIMD multiplication timing
    let simd_start = Instant::now();
    let _simd_result = test_matrix_a.multiply_simd(&test_matrix_b);
    let simd_duration = simd_start.elapsed();

    println!("  Scalar 16x16 matrix multiplication: {:?}", scalar_duration);
    println!("  SIMD 16x16 matrix multiplication:   {:?}", simd_duration);
    if scalar_duration > simd_duration {
        let speedup = scalar_duration.as_nanos() as f64 / simd_duration.as_nanos() as f64;
        println!("  🚀 SIMD speedup: {:.2}x faster!\n", speedup);
    } else {
        println!("  📊 Results may vary based on CPU architecture\n");
    }

    let start_time = Instant::now();

    // Create the workflow with SIMD-accelerated nodes
    let mut workflow = Workflow::new(PipelineState::Generate);

    // Register all pipeline nodes
    workflow
        .register_node(PipelineState::Generate, MatrixGenerator::new(64, 20)) // 64x64 matrices, 20 of them
        .register_node(PipelineState::Multiply, SimdMatrixMultiplier)
        .register_node(PipelineState::Transform, SimdMatrixTransformer::new(1.5))
        .register_node(PipelineState::Statistics, SimdStatisticsCalculator)
        .add_exit_states(vec![PipelineState::Complete, PipelineState::Error]);

    println!("🔧 Pipeline configured with {} nodes", workflow.state_nodes.len());
    println!("Pipeline: Generate → Multiply → Transform → Statistics → Complete\n");

    // Execute the workflow
    let store = MemoryStore::default();
    let _final_state = workflow.orchestrate(&store).await?;

    let total_duration = start_time.elapsed();
    println!("\n🎉 SIMD Matrix Processing Pipeline completed!");
    println!("Total execution time: {:?}", total_duration);
    
    if let Ok(stats) = store.get::<Vec<(f32, f32, f32)>>("statistics") {
        println!("📈 Final results: {} statistical summaries generated", stats.len());
    }

    println!("\n💡 This example showcased:");
    println!("  • Data generation in the first pipeline stage");
    println!("  • SIMD-accelerated matrix multiplication");
    println!("  • SIMD-accelerated element-wise operations");
    println!("  • SIMD-accelerated statistical calculations");
    println!("  • Processing 8 float32 values simultaneously using f32x8");
    println!("  • Three-phase node execution (prep, exec, post)");
    println!("  • Inter-node communication through shared store");

    Ok(())
}
