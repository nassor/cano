#[cfg(feature = "tracing")]
mod tests {
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

        let store = MemoryStore::new();
        let workflow = Workflow::new(store)
            .with_tracing_span(span)
            .register(TestState::Start, TestNode::new("start"))
            .register(TestState::Processing, TestNode::new("processing"))
            .add_exit_state(TestState::Complete);

        let result = workflow.orchestrate(TestState::Start).await.unwrap();

        assert_eq!(result, TestState::Complete);
    }

    #[tokio::test]
    async fn test_concurrent_workflow_with_tracing_span() {
        // ConcurrentWorkflow is replaced by split/join functionality in the new Workflow API
        let span = info_span!("test_concurrent_workflow", test_id = "concurrent_span_test");

        let store = MemoryStore::new();

        // Using split/join with JoinStrategy::All
        let workflow = Workflow::new(store)
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

            let store = MemoryStore::new();
            let workflow = Workflow::new(store)
                .with_tracing_span(span)
                .register(TestState::Start, TestNode::new("start"))
                .register(TestState::Processing, TestNode::new("processing"))
                .add_exit_state(TestState::Complete);

            let mut scheduler = Scheduler::new();
            scheduler
                .manual("test_workflow", workflow, TestState::Start)
                .unwrap();

            // Clone the scheduler before moving into spawn so we can stop it later.
            // Scheduler clones share the same internal Arc state.
            let scheduler_ref = scheduler.clone();

            // Spawn scheduler in background
            let scheduler_handle = tokio::spawn(async move { scheduler.start().await });

            // Poll until start() has initialised the command channel.
            // Using a short spin-sleep avoids a hard-coded delay that can
            // race on slow CI runners.
            let mut stop_result = scheduler_ref.stop().await;
            let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_millis(500);
            while stop_result.is_err() && tokio::time::Instant::now() < deadline {
                tokio::time::sleep(tokio::time::Duration::from_millis(5)).await;
                stop_result = scheduler_ref.stop().await;
            }

            // Check initial status via the shared clone
            let status = scheduler_ref.status("test_workflow").await;
            assert!(status.is_some());
            assert_eq!(status.unwrap().status, crate::scheduler::Status::Idle);

            stop_result.unwrap();

            // Wait for scheduler to finish
            let _ = tokio::time::timeout(tokio::time::Duration::from_millis(500), scheduler_handle)
                .await;
        })
        .await;

        assert!(result.is_ok(), "Scheduler test timed out");
    }

    #[tokio::test]
    async fn test_workflow_tracing_without_custom_span() {
        // Test that workflows work fine without custom spans when tracing is enabled
        let store = MemoryStore::new();
        let workflow = Workflow::new(store)
            .register(TestState::Start, TestNode::new("start"))
            .register(TestState::Processing, TestNode::new("processing"))
            .add_exit_state(TestState::Complete);

        let result = workflow.orchestrate(TestState::Start).await.unwrap();

        assert_eq!(result, TestState::Complete);
    }
}
