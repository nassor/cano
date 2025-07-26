# Concurrent Workflow Feature

## Overview

The Concurrent Workflow feature allows you to execute multiple workflow instances in parallel with configurable timeout strategies. This enables powerful parallel processing patterns while maintaining type safety and error handling.

## Key Components

### `ConcurrentTimeout` Enum

Defines different timeout strategies for concurrent execution:

```rust
enum ConcurrentTimeout {
    /// Wait indefinitely for all workflows to complete
    WaitForever,
    
    /// Wait for a specific number of workflows to complete (minimum 1)
    WaitForQuota(usize),
    
    /// Wait for a maximum duration, then cancel remaining workflows
    WaitDuration(Duration),
    
    /// Wait for either a quota OR a duration, whichever comes first
    WaitQuotaOrDuration { quota: usize, duration: Duration },
}
```

### `ConcurrentWorkflowInstance`

Represents a workflow instance to be executed concurrently, with optional parameters:

```rust
struct ConcurrentWorkflowInstance<TState, TStore, TParams> {
    pub workflow: Workflow<TState, TStore, TParams>,
    pub params: Option<TParams>,
    pub id: String,
}
```

### `ConcurrentWorkflowResults`

Aggregated results from concurrent workflow execution:

```rust
struct ConcurrentWorkflowResults<TState> {
    pub completed: usize,    // Number of successful workflows
    pub cancelled: usize,    // Number of cancelled workflows  
    pub failed: usize,       // Number of failed workflows
    pub results: Vec<WorkflowResult<TState>>,
    pub total_duration: Duration,
}
```

## Usage Examples

### Basic Concurrent Execution

```rust
use cano::{ConcurrentWorkflow, ConcurrentWorkflowInstance, ConcurrentTimeout};

// Create workflow instances
let instances = vec![
    ConcurrentWorkflowInstance::new("workflow_1".to_string(), workflow1),
    ConcurrentWorkflowInstance::new("workflow_2".to_string(), workflow2),
    ConcurrentWorkflowInstance::new("workflow_3".to_string(), workflow3),
];

// Execute all workflows concurrently
let concurrent_workflow = ConcurrentWorkflow::new(ConcurrentTimeout::WaitForever);
concurrent_workflow.add_instances(instances);

let results = concurrent_workflow.orchestrate(store).await?;

println!("Completed: {}, Failed: {}, Cancelled: {}", 
    results.completed, results.failed, results.cancelled);
```

### Quota-Based Execution

```rust
// Wait for first 3 workflows to complete, cancel the rest
let concurrent_workflow = ConcurrentWorkflow::new(ConcurrentTimeout::WaitForQuota(3));
```

### Time-Limited Execution

```rust
// Wait maximum 5 seconds, then cancel remaining workflows
let timeout = Duration::from_secs(5);
let concurrent_workflow = ConcurrentWorkflow::new(ConcurrentTimeout::WaitDuration(timeout));
```

### Hybrid Strategy

```rust
// Complete 3 workflows OR wait 10 seconds, whichever comes first
let concurrent_workflow = ConcurrentWorkflow::new(
    ConcurrentTimeout::WaitQuotaOrDuration {
        quota: 3,
        duration: Duration::from_secs(10),
    }
);
```

### Using the Builder Pattern

```rust
let concurrent_workflow = ConcurrentWorkflowBuilder::new(ConcurrentTimeout::WaitForever)
    .add_instance(instance1)
    .add_instance(instance2)
    .add_instances(vec![instance3, instance4])
    .build();
```

## Requirements

- `TStore` must implement `Clone + Send + Sync + 'static` for concurrent execution
- Workflow instances are consumed during execution (moved, not borrowed)
- Each instance can have different parameter values

## Benefits

1. **Parallel Processing**: Execute multiple workflows simultaneously
2. **Flexible Timeout Strategies**: Choose the right strategy for your use case
3. **Resource Management**: Cancel workflows when quotas or timeouts are reached
4. **Detailed Results**: Get comprehensive statistics about execution
5. **Type Safety**: Maintain compile-time safety with concurrent execution
6. **Parameter Variety**: Each instance can have different parameter values

## Use Cases

- Batch processing of similar tasks
- Fan-out processing patterns
- Time-sensitive workflow execution
- Resource-constrained parallel processing
- A/B testing with multiple workflow variants
