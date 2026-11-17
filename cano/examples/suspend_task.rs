//! # SuspendTask: a human-in-the-loop rules review
//!
//! Run with: `cargo run --example suspend_task --features recovery`
//!
//! An AI agent proposes a pricing rule set, a policy gate checks it, a human reviews it, and
//! the approved set is deployed:
//!
//! ```text
//!   Suggest ──► PolicyGate ──(router)──► Review ──(suspend)──► Deploy ──► Deployed
//!      ▲                 │                  │
//!      │                 └──► GateRejected  ├──► Rejected
//!      └────── RequestChanges ──────────────┘
//! ```
//!
//! `Review` is a [`SuspendTask`]: `orchestrate` returns `Ok(Step::Review)` once the
//! reviewer has been notified, and the run stays parked in the checkpoint store.
//! [`Workflow::resume_with`] delivers the reviewer's [`Decision`] later. The workflow is
//! rebuilt before every engine call to show that only the checkpoint store and the
//! application's own database (`RulesDb`) carry state across the pause.

use std::sync::Arc;

use cano::RedbCheckpointStore;
use cano::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Step {
    Suggest,
    PolicyGate,
    Review,
    Deploy,
    Deployed,
    GateRejected,
    Rejected,
}

/// The reviewer's verdict, delivered through `Workflow::resume_with`.
#[derive(Debug, Clone, Serialize, Deserialize)]
enum Decision {
    Approve,
    Reject { reason: String },
    RequestChanges { feedback: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RuleSet {
    revision: u32,
    max_discount_pct: u32,
}

#[derive(Debug, Default)]
struct RulesState {
    proposal: Option<RuleSet>,
    feedback: Option<String>,
    review_requests: u32,
    deployed: Option<RuleSet>,
}

/// Stands in for the application database: it outlives every workflow instance.
#[derive(Clone, Default)]
struct RulesDb(Arc<parking_lot::Mutex<RulesState>>);

#[resource]
impl Resource for RulesDb {}

/// The AI agent: proposes a rule set, tightening it when the reviewer left feedback.
struct SuggestRules;

#[task(state = Step)]
impl SuggestRules {
    async fn run(&self, res: &Resources) -> Result<Step, CanoError> {
        let db = res.get::<RulesDb>()?;
        let mut state = db.0.lock();
        let revision = state.proposal.as_ref().map_or(0, |p| p.revision) + 1;
        let max_discount_pct = if state.feedback.is_some() { 20 } else { 25 };
        println!("suggest    : revision {revision}, max discount {max_discount_pct}%");
        state.proposal = Some(RuleSet {
            revision,
            max_discount_pct,
        });
        Ok(Step::PolicyGate)
    }
}

/// The rules engine: a hard policy limit no proposal may exceed.
struct PolicyGate;

#[task::router(state = Step)]
impl PolicyGate {
    async fn route(&self, res: &Resources) -> Result<Step, CanoError> {
        let db = res.get::<RulesDb>()?;
        let state = db.0.lock();
        let proposal = state
            .proposal
            .as_ref()
            .ok_or_else(|| CanoError::task_execution("no proposal to check"))?;
        if proposal.max_discount_pct <= 30 {
            Ok(Step::Review)
        } else {
            Ok(Step::GateRejected)
        }
    }
}

/// The human review: notify on `suspend`, act on the decision in `resume`.
struct HumanReview;

#[task::suspend(state = Step)]
impl HumanReview {
    async fn suspend(&self, res: &Resources) -> Result<(), CanoError> {
        let db = res.get::<RulesDb>()?;
        let mut state = db.0.lock();
        state.review_requests += 1;
        let revision = state.proposal.as_ref().map_or(0, |p| p.revision);
        println!("review     : requested review of revision {revision}");
        Ok(())
    }

    async fn resume(&self, res: &Resources, decision: Decision) -> Result<Step, CanoError> {
        println!("review     : decision {decision:?}");
        Ok(match decision {
            Decision::Approve => Step::Deploy,
            Decision::Reject { .. } => Step::Rejected,
            Decision::RequestChanges { feedback } => {
                res.get::<RulesDb>()?.0.lock().feedback = Some(feedback);
                Step::Suggest
            }
        })
    }
}

/// Deploys the approved proposal.
struct DeployRules;

#[task(state = Step)]
impl DeployRules {
    async fn run(&self, res: &Resources) -> Result<Step, CanoError> {
        let db = res.get::<RulesDb>()?;
        let mut state = db.0.lock();
        state.deployed = state.proposal.clone();
        println!("deploy     : deployed {:?}", state.deployed);
        Ok(Step::Deployed)
    }
}

fn build(db: RulesDb, store: Arc<RedbCheckpointStore>, run_id: &str) -> Workflow<Step> {
    Workflow::new(Resources::new().insert(db))
        .register(Step::Suggest, SuggestRules)
        .register(Step::PolicyGate, PolicyGate)
        .register(Step::Review, HumanReview)
        .register(Step::Deploy, DeployRules)
        .add_exit_states([Step::Deployed, Step::GateRejected, Step::Rejected])
        .with_checkpoint_store(store)
        .with_workflow_id(run_id)
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    let store = Arc::new(RedbCheckpointStore::new(dir.path().join("rules.redb"))?);
    let db = RulesDb::default();
    let run_id = "rules-42";

    println!("── orchestrate: suggest, gate, request review ──");
    let state = build(db.clone(), store.clone(), run_id)
        .orchestrate(Step::Suggest)
        .await?;
    println!("  parked at {state:?}");
    assert_eq!(state, Step::Review);
    assert_eq!(db.0.lock().review_requests, 1);

    println!("\n── resume_from: no decision yet, nothing runs ──");
    let state = build(db.clone(), store.clone(), run_id)
        .resume_from(run_id)
        .await?;
    println!("  still parked at {state:?}");
    assert_eq!(state, Step::Review);
    assert_eq!(db.0.lock().review_requests, 1);

    println!("\n── resume_with: reviewer requests changes ──");
    let decision = Decision::RequestChanges {
        feedback: "cap discounts at 20%".into(),
    };
    let state = build(db.clone(), store.clone(), run_id)
        .resume_with(run_id, &decision)
        .await?;
    println!("  parked at {state:?}");
    assert_eq!(state, Step::Review);
    assert_eq!(db.0.lock().review_requests, 2);
    assert_eq!(db.0.lock().proposal.as_ref().map(|p| p.revision), Some(2));

    println!("\n── resume_with: reviewer approves ──");
    let state = build(db.clone(), store.clone(), run_id)
        .resume_with(run_id, &Decision::Approve)
        .await?;
    println!("  finished at {state:?}");
    assert_eq!(state, Step::Deployed);
    assert_eq!(
        db.0.lock().deployed.as_ref().map(|r| r.max_discount_pct),
        Some(20)
    );
    assert!(store.load_run(run_id).await?.is_empty());

    Ok(())
}
