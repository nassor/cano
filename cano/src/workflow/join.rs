//! Join strategies and configuration for parallel split tasks.
//!
//! [`Workflow::register_split`] runs several tasks in parallel; a [`JoinConfig`]
//! decides when the workflow may proceed past them and which state to enter next.
//! The [`SplitResult`] / [`SplitTaskResult`] types describe what came back.
//!
//! [`Workflow::register_split`]: crate::workflow::Workflow::register_split

use std::time::Duration;

use crate::error::CanoError;
use crate::task::TaskResult;

/// Strategy for joining parallel tasks
#[derive(Clone, Debug, PartialEq)]
pub enum JoinStrategy {
    /// All tasks must complete successfully
    All,
    /// Any single task completion triggers join
    Any,
    /// Specific number of tasks must complete
    Quorum(usize),
    /// A fraction of tasks must complete successfully.
    ///
    /// The value must be in the range `(0.0, 1.0]`. A value of `1.0` means all tasks
    /// must succeed (equivalent to [`All`](JoinStrategy::All)). Values outside this
    /// range return [`CanoError::Configuration`] when the split executes.
    Percentage(f64),
    /// Accept partial results - continues after minimum tasks complete successfully,
    /// cancels remaining tasks, and returns both successes and errors
    /// Parameter is the minimum number of tasks that must complete successfully
    PartialResults(usize),
    /// Accept whatever completes before the deadline, then cancel the rest.
    ///
    /// The join succeeds as long as at least one task (success **or** failure) finished
    /// before the timeout; the workflow continues with [`JoinConfig::join_state`].
    /// If zero tasks complete before the deadline, the workflow errors with
    /// [`CanoError::Workflow`].
    ///
    /// Unlike [`PartialResults`](JoinStrategy::PartialResults), which waits for a
    /// minimum number of *successful* completions, `PartialTimeout` is purely
    /// time-bounded and accepts any mixture of successes and failures.
    ///
    /// **Requires** a timeout to be set via [`JoinConfig::with_timeout`]; configuring
    /// this strategy without a timeout returns [`CanoError::Configuration`] at runtime.
    PartialTimeout,
}

impl JoinStrategy {
    /// Check if the join condition is met based on completed/total tasks
    pub fn is_satisfied(&self, completed: usize, total: usize) -> bool {
        match self {
            JoinStrategy::All => completed >= total,
            JoinStrategy::Any => completed >= 1,
            JoinStrategy::Quorum(n) => completed >= *n,
            JoinStrategy::Percentage(p) => {
                // Percentage must be in (0.0, 1.0] — validated at execute_split_join entry.
                // Saturate to usize::MAX rather than wrap; a task count large enough to
                // overflow f64→usize would OOM first anyway.
                let required_f = (total as f64 * p).ceil();
                let required = if required_f >= usize::MAX as f64 {
                    usize::MAX
                } else {
                    required_f as usize
                };
                completed >= required
            }
            JoinStrategy::PartialResults(min) => completed >= *min,
            JoinStrategy::PartialTimeout => completed >= 1, // At least one task must complete
        }
    }
}

/// Result of a single split task execution
#[derive(Clone, Debug)]
pub struct SplitTaskResult<TState> {
    /// Index of the task in the split tasks vector
    pub task_index: usize,
    /// Result of the task execution
    pub result: Result<TaskResult<TState>, CanoError>,
}

/// Collection of split task results with both successes and errors
#[derive(Clone, Debug)]
pub struct SplitResult<TState> {
    /// Successfully completed tasks
    pub successes: Vec<SplitTaskResult<TState>>,
    /// Failed tasks
    pub errors: Vec<SplitTaskResult<TState>>,
    /// Tasks that were cancelled (not started or aborted)
    pub cancelled: Vec<usize>,
}

impl<TState> SplitResult<TState> {
    /// Create a new empty split result
    pub fn new() -> Self {
        Self {
            successes: Vec::new(),
            errors: Vec::new(),
            cancelled: Vec::new(),
        }
    }

    /// Create a split result with capacity for `total_tasks` outcomes preallocated.
    /// Used internally by `collect_results` to avoid Vec resizes during collection.
    pub fn with_capacity(total_tasks: usize) -> Self {
        Self {
            successes: Vec::with_capacity(total_tasks),
            errors: Vec::with_capacity(total_tasks),
            cancelled: Vec::with_capacity(total_tasks),
        }
    }

    /// Total number of tasks that completed (success or error)
    pub fn completed_count(&self) -> usize {
        self.successes.len() + self.errors.len()
    }

    /// Total number of tasks including cancelled
    pub fn total_count(&self) -> usize {
        self.successes.len() + self.errors.len() + self.cancelled.len()
    }
}

impl<TState> Default for SplitResult<TState> {
    fn default() -> Self {
        Self::new()
    }
}

/// Configuration for join behavior after split tasks
#[must_use]
#[derive(Clone)]
pub struct JoinConfig<TState> {
    /// Strategy to determine when to proceed
    pub strategy: JoinStrategy,
    /// Optional timeout for the split execution
    pub timeout: Option<Duration>,
    /// State to transition to after join condition is met
    pub join_state: TState,
    /// Optional bulkhead: maximum number of split tasks allowed to run
    /// concurrently. When `None` (default) all tasks run as soon as the
    /// runtime can schedule them. When `Some(n)`, a `tokio::sync::Semaphore`
    /// with `n` permits gates each task body, so excess tasks queue until
    /// a permit is free. `Some(0)` is rejected at execution time with
    /// [`CanoError::Configuration`].
    pub bulkhead: Option<usize>,
}

impl<TState> JoinConfig<TState>
where
    TState: Clone,
{
    /// Create a new join configuration
    pub fn new(strategy: JoinStrategy, join_state: TState) -> Self {
        Self {
            strategy,
            timeout: None,
            join_state,
            bulkhead: None,
        }
    }

    /// Set timeout for the split execution
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    /// Set the state to transition to after the join condition is met
    pub fn with_join_state(mut self, state: TState) -> Self {
        self.join_state = state;
        self
    }

    /// Cap concurrent split task execution at `n`. `0` is rejected when the
    /// split runs.
    pub fn with_bulkhead(mut self, n: usize) -> Self {
        self.bulkhead = Some(n);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_join_strategy_is_satisfied() {
        assert!(JoinStrategy::All.is_satisfied(3, 3));
        assert!(!JoinStrategy::All.is_satisfied(2, 3));

        assert!(JoinStrategy::Any.is_satisfied(1, 3));
        assert!(!JoinStrategy::Any.is_satisfied(0, 3));

        assert!(JoinStrategy::Quorum(2).is_satisfied(2, 3));
        assert!(JoinStrategy::Quorum(2).is_satisfied(3, 3));
        assert!(!JoinStrategy::Quorum(2).is_satisfied(1, 3));

        assert!(JoinStrategy::Percentage(0.5).is_satisfied(2, 4));
        assert!(JoinStrategy::Percentage(0.75).is_satisfied(3, 4));
        assert!(!JoinStrategy::Percentage(0.75).is_satisfied(2, 4));

        assert!(JoinStrategy::PartialResults(2).is_satisfied(2, 4));
        assert!(JoinStrategy::PartialResults(2).is_satisfied(3, 4));
        assert!(!JoinStrategy::PartialResults(2).is_satisfied(1, 4));

        assert!(JoinStrategy::PartialTimeout.is_satisfied(1, 4));
        assert!(JoinStrategy::PartialTimeout.is_satisfied(3, 4));
        assert!(!JoinStrategy::PartialTimeout.is_satisfied(0, 4));
    }
}
