//! Integration tests for `#[cano::task::router]` — both the inherent form (which emits
//! `::cano::` paths) and the trait-impl form. These run outside the `cano` crate so
//! `::cano::` paths resolve correctly.

use cano::prelude::*;
use std::borrow::Cow;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Step {
    Route,
    PathA,
    PathB,
    Done,
}

// ---------------------------------------------------------------------------
// Inherent form: `#[task::router(state = S)] impl T { async fn route(...) }`
// ---------------------------------------------------------------------------

struct InherentRouter;

#[task::router(state = Step)]
impl InherentRouter {
    async fn route(&self, _res: &Resources) -> Result<TaskResult<Step>, CanoError> {
        Ok(TaskResult::Single(Step::PathA))
    }
}

#[test]
fn inherent_router_has_default_config() {
    assert_eq!(
        RouterTask::<Step>::config(&InherentRouter)
            .retry_mode
            .max_attempts(),
        4
    );
}

#[test]
fn inherent_router_has_default_name() {
    assert!(
        RouterTask::<Step>::name(&InherentRouter).contains("InherentRouter"),
        "default name should contain the type name"
    );
}

#[tokio::test]
async fn inherent_router_route_works() {
    let res = Resources::new();
    let result = RouterTask::route(&InherentRouter, &res).await.unwrap();
    assert_eq!(result, TaskResult::Single(Step::PathA));
}

#[tokio::test]
async fn inherent_router_task_run_works() {
    let res = Resources::new();
    let result = Task::run(&InherentRouter, &res).await.unwrap();
    assert_eq!(result, TaskResult::Single(Step::PathA));
}

// ---------------------------------------------------------------------------
// Inherent form with config/name overrides
// ---------------------------------------------------------------------------

struct InherentCustomRouter;

#[task::router(state = Step)]
impl InherentCustomRouter {
    fn config(&self) -> TaskConfig {
        TaskConfig::minimal()
    }

    fn name(&self) -> Cow<'static, str> {
        Cow::Borrowed("my-inherent-router")
    }

    async fn route(&self, _res: &Resources) -> Result<TaskResult<Step>, CanoError> {
        Ok(TaskResult::Single(Step::Done))
    }
}

#[test]
fn inherent_custom_config_override() {
    assert_eq!(
        RouterTask::<Step>::config(&InherentCustomRouter)
            .retry_mode
            .max_attempts(),
        1
    );
}

#[test]
fn inherent_custom_name_override() {
    assert_eq!(
        RouterTask::<Step>::name(&InherentCustomRouter),
        "my-inherent-router"
    );
}

#[test]
fn inherent_companion_task_forwards_config() {
    assert_eq!(
        Task::config(&InherentCustomRouter)
            .retry_mode
            .max_attempts(),
        1
    );
}

#[test]
fn inherent_companion_task_forwards_name() {
    assert_eq!(Task::name(&InherentCustomRouter), "my-inherent-router");
}

// ---------------------------------------------------------------------------
// Inherent form returning Split
// ---------------------------------------------------------------------------

struct InherentSplitRouter;

#[task::router(state = Step)]
impl InherentSplitRouter {
    async fn route(&self, _res: &Resources) -> Result<TaskResult<Step>, CanoError> {
        Ok(TaskResult::Split(vec![Step::PathA, Step::PathB]))
    }
}

#[tokio::test]
async fn inherent_router_can_return_split() {
    let res = Resources::new();
    let result = Task::run(&InherentSplitRouter, &res).await.unwrap();
    assert_eq!(result, TaskResult::Split(vec![Step::PathA, Step::PathB]));
}

// ---------------------------------------------------------------------------
// Trait-impl form: `#[task::router] impl RouterTask<S> for T { ... }`
// ---------------------------------------------------------------------------

struct TraitRouter;

#[task::router]
impl RouterTask<Step> for TraitRouter {
    async fn route(&self, _res: &Resources) -> Result<TaskResult<Step>, CanoError> {
        Ok(TaskResult::Single(Step::PathA))
    }
}

#[tokio::test]
async fn trait_form_route_works() {
    let res = Resources::new();
    let result = RouterTask::route(&TraitRouter, &res).await.unwrap();
    assert_eq!(result, TaskResult::Single(Step::PathA));
}

#[tokio::test]
async fn trait_form_companion_task_run_works() {
    let res = Resources::new();
    let result = Task::run(&TraitRouter, &res).await.unwrap();
    assert_eq!(result, TaskResult::Single(Step::PathA));
}

// ---------------------------------------------------------------------------
// Dynamic dispatch: cast to Arc<dyn Task<Step>>
// ---------------------------------------------------------------------------

#[tokio::test]
async fn inherent_router_as_dyn_task() {
    use std::sync::Arc;
    let router: Arc<dyn Task<Step>> = Arc::new(InherentRouter);
    let res = Resources::new();
    let result = Task::run(router.as_ref(), &res).await.unwrap();
    assert_eq!(result, TaskResult::Single(Step::PathA));
}

// ---------------------------------------------------------------------------
// Workflow integration: register via Workflow::register
// ---------------------------------------------------------------------------

struct PathATask;

#[task(state = Step)]
impl PathATask {
    async fn run_bare(&self) -> Result<TaskResult<Step>, CanoError> {
        Ok(TaskResult::Single(Step::Done))
    }
}

#[tokio::test]
async fn inherent_router_integrates_with_workflow() {
    let workflow = Workflow::bare()
        .register(Step::Route, InherentRouter)
        .register(Step::PathA, PathATask)
        .add_exit_state(Step::Done);

    let result = workflow.orchestrate(Step::Route).await.unwrap();
    assert_eq!(result, Step::Done);
}

#[tokio::test]
async fn trait_router_integrates_with_workflow() {
    let workflow = Workflow::bare()
        .register(Step::Route, TraitRouter)
        .register(Step::PathA, PathATask)
        .add_exit_state(Step::Done);

    let result = workflow.orchestrate(Step::Route).await.unwrap();
    assert_eq!(result, Step::Done);
}
