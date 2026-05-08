#[cfg(feature = "tracing")]
mod tests {
    use crate::prelude::*;
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
}
