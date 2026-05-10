#[cfg(feature = "tracing")]
mod tests {
    use crate::prelude::*;
    use std::borrow::Cow;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;
    use tracing::info_span;

    #[derive(Debug, Clone, PartialEq, Eq, Hash)]
    enum TestState {
        Start,
        Processing,
        Complete,
    }

    #[derive(Clone)]
    struct TestNode {
        id: String,
    }

    impl TestNode {
        fn new(id: &str) -> Self {
            Self { id: id.to_string() }
        }
    }

    #[crate::node]
    impl Node<TestState> for TestNode {
        type PrepResult = String;
        type ExecResult = String;

        async fn prep(&self, _res: &Resources) -> Result<Self::PrepResult, CanoError> {
            Ok(format!("prep_{}", self.id))
        }

        async fn exec(&self, prep_result: Self::PrepResult) -> Self::ExecResult {
            format!("exec_{}", prep_result)
        }

        async fn post(
            &self,
            _res: &Resources,
            exec_result: Self::ExecResult,
        ) -> Result<TestState, CanoError> {
            if exec_result.contains("processing") {
                Ok(TestState::Complete)
            } else {
                Ok(TestState::Processing)
            }
        }
    }

    #[tokio::test]
    async fn test_workflow_with_tracing_span() {
        let span = info_span!("test_workflow", test_id = "workflow_span_test");

        let workflow = Workflow::new(Resources::new())
            .with_tracing_span(span)
            .register(TestState::Start, TestNode::new("start"))
            .register(TestState::Processing, TestNode::new("processing"))
            .add_exit_state(TestState::Complete);

        let result = workflow.orchestrate(TestState::Start).await.unwrap();

        assert_eq!(result, TestState::Complete);
    }

    #[tokio::test]
    async fn test_concurrent_workflow_with_tracing_span() {
        let span = info_span!("test_concurrent_workflow", test_id = "concurrent_span_test");

        // Using split/join with JoinStrategy::All
        let workflow = Workflow::new(Resources::new())
            .with_tracing_span(span)
            .register_split(
                TestState::Start,
                vec![TestNode::new("start1"), TestNode::new("start2")],
                JoinConfig::new(JoinStrategy::All, TestState::Processing),
            )
            .register(TestState::Processing, TestNode::new("processing"))
            .add_exit_state(TestState::Complete);

        let result = workflow.orchestrate(TestState::Start).await.unwrap();

        assert_eq!(result, TestState::Complete);
    }

    #[tokio::test(flavor = "multi_thread")]
    #[cfg(feature = "scheduler")]
    async fn test_scheduler_with_tracing() {
        let timeout = tokio::time::Duration::from_secs(2);
        let result = tokio::time::timeout(timeout, async {
            let span = info_span!("test_scheduler_workflow", test_id = "scheduler_span_test");

            let workflow = Workflow::new(Resources::new())
                .with_tracing_span(span)
                .register(TestState::Start, TestNode::new("start"))
                .register(TestState::Processing, TestNode::new("processing"))
                .add_exit_state(TestState::Complete);

            let mut scheduler = Scheduler::new();
            scheduler
                .manual("test_workflow", workflow, TestState::Start)
                .unwrap();

            // Start consumes the builder and returns a clone-able handle.
            let running = scheduler.start().await.unwrap();

            // Inspect status before stopping.
            let status = running.status("test_workflow").await;
            assert!(status.is_some());
            assert_eq!(status.unwrap().status, crate::scheduler::Status::Idle);

            running.stop().await.unwrap();
        })
        .await;

        assert!(result.is_ok(), "Scheduler test timed out");
    }

    #[tokio::test]
    async fn test_workflow_tracing_without_custom_span() {
        // Test that workflows work fine without custom spans when tracing is enabled
        let workflow = Workflow::new(Resources::new())
            .register(TestState::Start, TestNode::new("start"))
            .register(TestState::Processing, TestNode::new("processing"))
            .add_exit_state(TestState::Complete);

        let result = workflow.orchestrate(TestState::Start).await.unwrap();

        assert_eq!(result, TestState::Complete);
    }

    // -- TracingObserver --------------------------------------------------

    #[derive(Clone, Debug, PartialEq, Eq, Hash)]
    enum Flow {
        A,
        B,
        Done,
    }

    struct AlwaysFail;
    #[crate::task]
    impl Task<Flow> for AlwaysFail {
        fn config(&self) -> TaskConfig {
            TaskConfig::new().with_fixed_retry(2, Duration::from_millis(1))
        }
        fn name(&self) -> Cow<'static, str> {
            "always-fail".into()
        }
        async fn run_bare(&self) -> Result<TaskResult<Flow>, CanoError> {
            Err(CanoError::task_execution("nope"))
        }
    }

    struct RecoverAfter {
        remaining_failures: Arc<AtomicUsize>,
    }
    #[crate::task]
    impl Task<Flow> for RecoverAfter {
        fn config(&self) -> TaskConfig {
            TaskConfig::new().with_fixed_retry(3, Duration::from_millis(1))
        }
        fn name(&self) -> Cow<'static, str> {
            "recover-after".into()
        }
        async fn run_bare(&self) -> Result<TaskResult<Flow>, CanoError> {
            if self.remaining_failures.fetch_sub(1, Ordering::SeqCst) > 0 {
                return Err(CanoError::task_execution("not ready"));
            }
            Ok(TaskResult::Single(Flow::Done))
        }
    }

    struct GuardedTask {
        breaker: Arc<CircuitBreaker>,
    }
    #[crate::task]
    impl Task<Flow> for GuardedTask {
        fn config(&self) -> TaskConfig {
            TaskConfig::minimal().with_circuit_breaker(Arc::clone(&self.breaker))
        }
        fn name(&self) -> Cow<'static, str> {
            "guarded".into()
        }
        async fn run_bare(&self) -> Result<TaskResult<Flow>, CanoError> {
            Ok(TaskResult::Single(Flow::Done))
        }
    }

    #[tokio::test]
    async fn test_tracing_observer_runs_workflow() {
        // Behavior parity: attaching a TracingObserver doesn't change execution.
        let workflow = Workflow::bare()
            .register(
                Flow::A,
                RecoverAfter {
                    remaining_failures: Arc::new(AtomicUsize::new(1)),
                },
            )
            .add_exit_state(Flow::Done)
            .with_observer(Arc::new(TracingObserver::new()));
        assert_eq!(workflow.orchestrate(Flow::A).await.unwrap(), Flow::Done);
    }

    #[derive(Clone)]
    struct CaptureWriter(Arc<Mutex<Vec<u8>>>);
    impl std::io::Write for CaptureWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for CaptureWriter {
        type Writer = CaptureWriter;
        fn make_writer(&'a self) -> Self::Writer {
            self.clone()
        }
    }

    #[tokio::test]
    async fn test_tracing_observer_captures_events() {
        // `#[tokio::test]` uses the current-thread runtime, so a thread-local
        // subscriber set via `set_default` applies to everything the workflow runs.
        let buf = Arc::new(Mutex::new(Vec::<u8>::new()));
        let subscriber = tracing_subscriber::fmt()
            .with_writer(CaptureWriter(Arc::clone(&buf)))
            .with_max_level(tracing::Level::TRACE)
            .with_ansi(false)
            .finish();
        let guard = tracing::subscriber::set_default(subscriber);

        // Retry then ultimate failure → on_state_enter / on_task_start / on_retry / on_task_failure.
        let fail_wf = Workflow::bare()
            .register(Flow::A, AlwaysFail)
            .add_exit_state(Flow::Done)
            .with_observer(Arc::new(TracingObserver::new()));
        assert!(fail_wf.orchestrate(Flow::A).await.is_err());

        // Pre-tripped breaker → on_circuit_open + on_task_failure.
        let breaker = Arc::new(CircuitBreaker::new(CircuitPolicy {
            failure_threshold: 1,
            reset_timeout: Duration::from_secs(60),
            half_open_max_calls: 1,
        }));
        let permit = breaker.try_acquire().unwrap();
        breaker.record_failure(permit);
        let cb_wf = Workflow::bare()
            .register(
                Flow::B,
                GuardedTask {
                    breaker: Arc::clone(&breaker),
                },
            )
            .add_exit_state(Flow::Done)
            .with_observer(Arc::new(TracingObserver::new()));
        assert!(matches!(
            cb_wf.orchestrate(Flow::B).await,
            Err(CanoError::CircuitOpen(_))
        ));

        drop(guard);
        let output = String::from_utf8(buf.lock().unwrap().clone()).unwrap();
        assert!(
            output.contains("cano::observer"),
            "missing target:\n{output}"
        );
        assert!(
            output.contains("workflow entered state"),
            "missing state-enter event:\n{output}"
        );
        assert!(
            output.contains("task started"),
            "missing task-start event:\n{output}"
        );
        assert!(
            output.contains("task retry"),
            "missing retry event:\n{output}"
        );
        assert!(
            output.contains("always-fail"),
            "missing failing task id:\n{output}"
        );
        assert!(
            output.contains("task failed"),
            "missing task-failure event:\n{output}"
        );
        assert!(
            output.contains("circuit breaker rejected task"),
            "missing circuit-open event:\n{output}"
        );
        assert!(
            output.contains("guarded"),
            "missing guarded task id:\n{output}"
        );
    }
}
