#[cfg(feature = "tracing")]
mod tracing_tests {
    use crate::prelude::*;
    use async_trait::async_trait;
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

    #[async_trait]
    impl Node<TestState> for TestNode {
        type PrepResult = String;
        type ExecResult = String;

        async fn prep(&self, _store: &MemoryStore) -> Result<Self::PrepResult, CanoError> {
            Ok(format!("prep_{}", self.id))
        }

        async fn exec(&self, prep_result: Self::PrepResult) -> Self::ExecResult {
            format!("exec_{}", prep_result)
        }

        async fn post(
            &self,
            _store: &MemoryStore,
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

        let mut workflow = Workflow::new(TestState::Start).with_tracing_span(span);

        workflow
            .register(TestState::Start, TestNode::new("start"))
            .register(TestState::Processing, TestNode::new("processing"))
            .add_exit_state(TestState::Complete);

        let store = MemoryStore::new();
        let result = workflow.orchestrate(&store).await.unwrap();

        assert_eq!(result, TestState::Complete);
    }

    #[tokio::test]
    async fn test_concurrent_workflow_with_tracing_span() {
        let span = info_span!("test_concurrent_workflow", test_id = "concurrent_span_test");

        let mut concurrent_workflow =
            ConcurrentWorkflow::new(TestState::Start).with_tracing_span(span);

        concurrent_workflow
            .register(TestState::Start, TestNode::new("start"))
            .register(TestState::Processing, TestNode::new("processing"))
            .add_exit_state(TestState::Complete);

        let stores = vec![MemoryStore::new(), MemoryStore::new()];
        let (results, status) = concurrent_workflow
            .execute_concurrent(stores, WaitStrategy::WaitForever)
            .await
            .unwrap();

        assert_eq!(results.len(), 2);
        assert_eq!(status.completed, 2);
        assert_eq!(status.failed, 0);
    }

    #[tokio::test]
    #[cfg(feature = "scheduler")]
    async fn test_scheduler_with_tracing() {
        let span = info_span!("test_scheduler_workflow", test_id = "scheduler_span_test");

        let mut workflow = Workflow::new(TestState::Start).with_tracing_span(span);

        workflow
            .register(TestState::Start, TestNode::new("start"))
            .register(TestState::Processing, TestNode::new("processing"))
            .add_exit_state(TestState::Complete);

        let mut scheduler = Scheduler::new();
        scheduler.manual("test_workflow", workflow).unwrap();

        scheduler.start().await.unwrap();
        scheduler.trigger("test_workflow").await.unwrap();

        // Give some time for execution
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

        let status = scheduler.status("test_workflow").await;
        assert!(status.is_some());
        assert_eq!(status.unwrap().run_count, 1);

        scheduler.stop().await.unwrap();
    }

    #[tokio::test]
    async fn test_workflow_tracing_without_custom_span() {
        // Test that workflows work fine without custom spans when tracing is enabled
        let mut workflow = Workflow::new(TestState::Start);

        workflow
            .register(TestState::Start, TestNode::new("start"))
            .register(TestState::Processing, TestNode::new("processing"))
            .add_exit_state(TestState::Complete);

        let store = MemoryStore::new();
        let result = workflow.orchestrate(&store).await.unwrap();

        assert_eq!(result, TestState::Complete);
    }
}
