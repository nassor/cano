//! UI tests for `#[cano::task::node]` type inference on `impl Node` blocks.
//!
//! Verifies that:
//! - `type PrepResult` is inferred from `prep`'s `Result<T, _>` return type.
//! - `type ExecResult` is inferred from `exec`'s return type.
//! - `fn config` is injected with the default when absent.
//! - Explicit associated types override inference.

use cano::prelude::*;

// ── State type shared by all tests ───────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Step {
    Run,
    Done,
}

// ── Test 1: basic inference — prep returns Result<Vec<u32>, _>, exec returns Vec<u32> ──

struct BasicNode;

#[task::node]
impl Node<Step> for BasicNode {
    // No `type PrepResult` or `type ExecResult` — the macro infers them.

    async fn prep(&self, _res: &Resources) -> Result<Vec<u32>, CanoError> {
        Ok(vec![1, 2, 3])
    }

    async fn exec(&self, prep_res: Vec<u32>) -> Vec<u32> {
        prep_res.into_iter().map(|x| x * 2).collect()
    }

    async fn post(&self, _res: &Resources, exec_res: Vec<u32>) -> Result<Step, CanoError> {
        assert_eq!(exec_res, vec![2, 4, 6]);
        Ok(Step::Done)
    }
}

#[tokio::test]
async fn basic_inference_compiles_and_runs() {
    let resources = Resources::new();
    let workflow = Workflow::new(resources)
        .register(Step::Run, BasicNode)
        .add_exit_state(Step::Done);
    let result = workflow.orchestrate(Step::Run).await.unwrap();
    assert_eq!(result, Step::Done);
}

// ── Test 2: inference with CanoResult<T> alias ────────────────────────────────

struct CanoResultNode;

#[task::node]
impl Node<Step> for CanoResultNode {
    async fn prep(&self, _res: &Resources) -> CanoResult<String> {
        Ok("hello".to_string())
    }

    async fn exec(&self, prep_res: String) -> String {
        format!("{prep_res} world")
    }

    async fn post(&self, _res: &Resources, exec_res: String) -> Result<Step, CanoError> {
        assert_eq!(exec_res, "hello world");
        Ok(Step::Done)
    }
}

#[tokio::test]
async fn cano_result_alias_inference_works() {
    let resources = Resources::new();
    let workflow = Workflow::new(resources)
        .register(Step::Run, CanoResultNode)
        .add_exit_state(Step::Done);
    let result = workflow.orchestrate(Step::Run).await.unwrap();
    assert_eq!(result, Step::Done);
}

// ── Test 3: override — explicit type wins over inference ──────────────────────

struct OverrideNode;

#[task::node]
impl Node<Step> for OverrideNode {
    // Explicit associated types — inference is skipped for these.
    // (The macro only infers when types are absent, so this routes through
    //  the plain async rewriter in the lib dispatch, but still compiles.)
    type PrepResult = String;
    type ExecResult = String;

    async fn prep(&self, _res: &Resources) -> Result<Self::PrepResult, CanoError> {
        Ok("explicit".to_string())
    }

    async fn exec(&self, prep_res: Self::PrepResult) -> Self::ExecResult {
        prep_res
    }

    async fn post(&self, _res: &Resources, exec_res: Self::ExecResult) -> Result<Step, CanoError> {
        assert_eq!(exec_res, "explicit");
        Ok(Step::Done)
    }
}

#[tokio::test]
async fn explicit_types_are_preserved() {
    let resources = Resources::new();
    let workflow = Workflow::new(resources)
        .register(Step::Run, OverrideNode)
        .add_exit_state(Step::Done);
    let result = workflow.orchestrate(Step::Run).await.unwrap();
    assert_eq!(result, Step::Done);
}

// ── Test 4: default config is injected ───────────────────────────────────────

struct DefaultConfigNode;

#[task::node]
impl Node<Step> for DefaultConfigNode {
    // No `fn config` — the macro should inject the default.

    async fn prep(&self, _res: &Resources) -> Result<u32, CanoError> {
        Ok(42)
    }

    async fn exec(&self, prep_res: u32) -> u32 {
        prep_res
    }

    async fn post(&self, _res: &Resources, exec_res: u32) -> Result<Step, CanoError> {
        assert_eq!(exec_res, 42);
        Ok(Step::Done)
    }
}

#[tokio::test]
async fn default_config_is_injected_and_works() {
    let resources = Resources::new();
    let workflow = Workflow::new(resources)
        .register(Step::Run, DefaultConfigNode)
        .add_exit_state(Step::Done);
    let result = workflow.orchestrate(Step::Run).await.unwrap();
    assert_eq!(result, Step::Done);
}
