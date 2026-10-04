//! Regression: cano's `tracing` spans must never stay entered while a workflow is suspended.
//!
//! Spans that wrap async work must be attached with `Instrument::instrument` (entered only while
//! the future is polled). A `Span::enter()` guard held across an `.await` leaves the span entered
//! on the worker thread while other tasks run there: unrelated tasks inherit it (orphaned and
//! mis-nested traces) and, on a multi-thread runtime, the registry can panic ("tried to clone a
//! span that already closed").
//!
//! Every scenario runs on both runtime flavors, and neither is redundant. On a current-thread
//! runtime the bystander task shares the one thread with every suspended workflow, so a span
//! left entered is always visible to it: deterministic leak detection. On a multi-thread runtime
//! the bystander only sees a leak on the worker thread it happens to run on, so detection there
//! rests mainly on the nesting and span-close checks (and on the registry panic a leaked guard
//! can trigger when work migrates between workers).
//!
//! Requires the `tracing` feature; the `scheduled` scenario also needs `scheduler`.
#![cfg(feature = "tracing")]

mod support;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, Once, PoisonError};
use std::time::{Duration, Instant};

use cano::prelude::*;
use futures_util::future::BoxFuture;
use support::MemStore;
use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id};
use tracing::{Event, Instrument, Subscriber};
use tracing_subscriber::Layer;
use tracing_subscriber::layer::{Context, SubscriberExt};
use tracing_subscriber::registry::LookupSpan;

// ---------------------------------------------------------------------------
// Capture layer: records every `owner`-tagged event with its span scope, and which runs' spans
// are still open.
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct SpanMeta {
    name: &'static str,
    fields: HashMap<&'static str, String>,
    /// The run (`workflow_id`) this span belongs to: its own, else its parent's.
    run: Option<String>,
}

struct EventRecord {
    fields: HashMap<&'static str, String>,
    /// Spans the event sits in, innermost first.
    scope: Vec<SpanMeta>,
}

#[derive(Default)]
struct Fields(HashMap<&'static str, String>);

impl Visit for Fields {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        self.0.insert(field.name(), format!("{value:?}"));
    }
    fn record_str(&mut self, field: &Field, value: &str) {
        self.0.insert(field.name(), value.to_owned());
    }
    fn record_i64(&mut self, field: &Field, value: i64) {
        self.0.insert(field.name(), value.to_string());
    }
    fn record_u64(&mut self, field: &Field, value: u64) {
        self.0.insert(field.name(), value.to_string());
    }
}

static EVENTS: Mutex<Vec<EventRecord>> = Mutex::new(Vec::new());
/// Spans opened and not yet closed, by id: `(run, span name)`.
static OPEN: Mutex<BTreeMap<u64, (String, &'static str)>> = Mutex::new(BTreeMap::new());
static INSTALL: Once = Once::new();

fn events() -> MutexGuard<'static, Vec<EventRecord>> {
    EVENTS.lock().unwrap_or_else(PoisonError::into_inner)
}

fn open_spans() -> MutexGuard<'static, BTreeMap<u64, (String, &'static str)>> {
    OPEN.lock().unwrap_or_else(PoisonError::into_inner)
}

struct CaptureLayer;

impl<S> Layer<S> for CaptureLayer
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_new_span(&self, attrs: &Attributes<'_>, id: &Id, ctx: Context<'_, S>) {
        let mut fields = Fields::default();
        attrs.record(&mut fields);
        let span = ctx.span(id).expect("a span being created exists");
        let run = fields.0.get("workflow_id").cloned().or_else(|| {
            let parent = span.parent()?;
            let ext = parent.extensions();
            ext.get::<SpanMeta>()?.run.clone()
        });
        if let Some(run) = &run {
            open_spans().insert(id.into_u64(), (run.clone(), span.name()));
        }
        span.extensions_mut().insert(SpanMeta {
            name: span.name(),
            fields: fields.0,
            run,
        });
    }

    fn on_close(&self, id: Id, _ctx: Context<'_, S>) {
        open_spans().remove(&id.into_u64());
    }

    fn on_event(&self, event: &Event<'_>, ctx: Context<'_, S>) {
        let mut fields = Fields::default();
        event.record(&mut fields);
        // Only the probes' events matter; cano's own events carry no `owner` field.
        if !fields.0.contains_key("owner") {
            return;
        }
        let scope = ctx
            .event_scope(event)
            .into_iter()
            .flatten()
            .map(|span| {
                let ext = span.extensions();
                ext.get::<SpanMeta>()
                    .cloned()
                    .expect("every span has SpanMeta")
            })
            .collect();
        events().push(EventRecord {
            fields: fields.0,
            scope,
        });
    }
}

fn install_capture() {
    INSTALL.call_once(|| {
        tracing::subscriber::set_global_default(tracing_subscriber::registry().with(CaptureLayer))
            .expect("this test binary installs the only global subscriber");
    });
}

// ---------------------------------------------------------------------------
// Task bodies
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum S {
    Start,
    Mid,
    Done,
}

/// What a [`Probe`] does on a given attempt after its second event.
#[derive(Clone, Copy)]
enum Act {
    /// Finish normally.
    Ok,
    /// Return an error (the retry policy decides what happens next).
    Fail,
    /// Suspend forever: only a drop (attempt timeout, aborted branch, cancel) ends it.
    Hang,
    /// Fire the workflow's cancellation handle, then suspend forever.
    Cancel,
}

/// A task body that emits events from inside cano's span tree and suspends at the awaits where
/// a leaked span guard would be observed.
#[derive(Clone)]
struct Probe {
    /// Workflow id the events are attributed to; empty = emit nothing.
    owner: String,
    /// Which cano span must wrap this task: "single" | "split" | "compensatable". `compensate`
    /// events use kind "compensate" instead (see `emit_compensation`).
    kind: &'static str,
    /// Split branch index; -1 outside split states.
    branch: i64,
    next: S,
    /// First call across all clones fails (leaves a checkpoint to resume from).
    fail_once: Option<Arc<AtomicU32>>,
    /// Retry / timeout policy cano runs this task under.
    config: TaskConfig,
    /// What attempt `n` does: `script[n - 1]`, `Act::Ok` past the end.
    script: &'static [Act],
    cancel: Option<CancellationHandle>,
    /// Calls so far (shared by clones): the 1-based attempt ordinal the events carry.
    calls: Arc<AtomicU32>,
    /// All parties have started (and emitted twice) before any of them goes on.
    rendezvous: Option<Arc<tokio::sync::Barrier>>,
}

/// Emits a `dropped` event if the attempt that created it is dropped before it finishes
/// (attempt timeout, aborted branch, cancelled run): cleanup logs must keep their span context.
struct DropWatch<'a> {
    probe: &'a Probe,
    attempt: u32,
    finished: bool,
}

impl Drop for DropWatch<'_> {
    fn drop(&mut self) {
        if !self.finished {
            self.probe.emit(self.attempt, "dropped");
        }
    }
}

impl Probe {
    fn new(owner: &str, kind: &'static str, branch: i64, next: S) -> Self {
        Self {
            owner: owner.to_owned(),
            kind,
            branch,
            next,
            fail_once: None,
            config: TaskConfig::minimal(),
            script: &[],
            cancel: None,
            calls: Arc::new(AtomicU32::new(0)),
            rendezvous: None,
        }
    }

    fn failing_once(mut self, gate: Arc<AtomicU32>) -> Self {
        self.fail_once = Some(gate);
        self
    }

    fn with_config(mut self, config: TaskConfig) -> Self {
        self.config = config;
        self
    }

    fn with_script(mut self, script: &'static [Act]) -> Self {
        self.script = script;
        self
    }

    fn cancelling(mut self, handle: CancellationHandle) -> Self {
        self.cancel = Some(handle);
        self
    }

    fn rendezvous_at(mut self, barrier: Arc<tokio::sync::Barrier>) -> Self {
        self.rendezvous = Some(barrier);
        self
    }

    fn emit(&self, attempt: u32, step: &'static str) {
        if !self.owner.is_empty() {
            tracing::info!(
                owner = %self.owner,
                kind = self.kind,
                branch = self.branch,
                attempt,
                max_attempts = self.config.retry_mode.max_attempts() as u64,
                step,
                "probe"
            );
        }
    }

    /// An event from `compensate`, which cano runs inside the workflow's spans but outside any
    /// task span.
    fn emit_compensation(&self, step: &'static str) {
        if !self.owner.is_empty() {
            tracing::info!(
                owner = %self.owner,
                kind = "compensate",
                branch = -1i64,
                attempt = 0u32,
                max_attempts = 0u64,
                step,
                "probe"
            );
        }
    }

    async fn body(&self) -> Result<S, CanoError> {
        if let Some(gate) = &self.fail_once
            && gate.fetch_add(1, Ordering::SeqCst) == 0
        {
            return Err(CanoError::task_execution("planned first-run failure"));
        }
        let attempt = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        let mut watch = DropWatch {
            probe: self,
            attempt,
            finished: false,
        };
        self.emit(attempt, "start");
        tokio::task::yield_now().await;
        self.emit(attempt, "resumed");
        if let Some(barrier) = &self.rendezvous {
            barrier.wait().await;
        }
        match self
            .script
            .get(attempt as usize - 1)
            .copied()
            .unwrap_or(Act::Ok)
        {
            Act::Ok => {}
            Act::Fail => {
                watch.finished = true;
                return Err(CanoError::task_execution("scripted failure"));
            }
            Act::Hang => std::future::pending::<()>().await,
            Act::Cancel => {
                self.cancel
                    .as_ref()
                    .expect("a cancelling probe has a handle")
                    .cancel();
                std::future::pending::<()>().await
            }
        }
        tokio::time::sleep(Duration::from_millis(1)).await;
        self.emit(attempt, "done");
        watch.finished = true;
        Ok(self.next.clone())
    }
}

#[cano::task]
impl Task<S> for Probe {
    fn config(&self) -> TaskConfig {
        self.config.clone()
    }
    async fn run_bare(&self) -> Result<TaskResult<S>, CanoError> {
        self.body().await.map(TaskResult::Single)
    }
}

/// The same body as a saga step. Its `compensate` emits "compensate" events (see
/// `Probe::emit_compensation`); only the `cancelled` rollback exercises it.
#[derive(Clone)]
struct CompProbe(Probe);

#[saga::task(state = S)]
impl CompProbe {
    type Output = ();
    fn config(&self) -> TaskConfig {
        TaskConfig::minimal()
    }
    async fn run(&self, _res: &Resources) -> Result<(TaskResult<S>, ()), CanoError> {
        Ok((TaskResult::Single(self.0.body().await?), ()))
    }
    async fn compensate(&self, _res: &Resources, _output: ()) -> Result<(), CanoError> {
        self.0.emit_compensation("start");
        tokio::task::yield_now().await;
        self.0.emit_compensation("resumed");
        tokio::time::sleep(Duration::from_millis(1)).await;
        self.0.emit_compensation("done");
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Scenarios: each builds and runs one workflow for `id`.
// ---------------------------------------------------------------------------

type Run = BoxFuture<'static, Result<S, CanoError>>;

/// orchestrate + single_task_execution + task_attempt
fn single(id: String) -> Run {
    Box::pin(async move {
        Workflow::bare()
            .register(S::Start, Probe::new(&id, "single", -1, S::Mid))
            .register(S::Mid, Probe::new(&id, "single", -1, S::Done))
            .add_exit_state(S::Done)
            .with_workflow_id(id.as_str())
            .orchestrate(S::Start, CancellationToken::disabled())
            .await
    })
}

/// The `split` topology: two parallel branches, then a single state.
fn split_workflow(id: &str) -> Workflow<S> {
    Workflow::bare()
        .register_split(
            S::Start,
            vec![
                Probe::new(id, "split", 0, S::Mid),
                Probe::new(id, "split", 1, S::Mid),
            ],
            JoinConfig::new(JoinStrategy::All, S::Mid),
        )
        .register(S::Mid, Probe::new(id, "single", -1, S::Done))
        .add_exit_state(S::Done)
        .with_workflow_id(id)
}

/// split_task (two parallel branches, then a single state)
fn split(id: String) -> Run {
    Box::pin(async move {
        split_workflow(&id)
            .orchestrate(S::Start, CancellationToken::disabled())
            .await
    })
}

/// compensatable_task_execution
fn compensatable(id: String) -> Run {
    Box::pin(async move {
        Workflow::bare()
            .register_with_compensation(
                S::Start,
                CompProbe(Probe::new(&id, "compensatable", -1, S::Mid)),
            )
            .register_with_compensation(
                S::Mid,
                CompProbe(Probe::new(&id, "compensatable", -1, S::Done)),
            )
            .add_exit_state(S::Done)
            .with_workflow_id(id.as_str())
            .orchestrate(S::Start, CancellationToken::disabled())
            .await
    })
}

/// A first (silent) run fails in `Mid` and leaves a checkpoint; the resumed run re-executes
/// `Mid` and is the one whose events are checked. `user_span` puts the resumed run under a
/// caller-supplied root instead of cano's own.
fn resumed(id: String, user_span: Option<tracing::Span>) -> Run {
    Box::pin(async move {
        let store = Arc::new(MemStore::default());
        let gate = Arc::new(AtomicU32::new(0));
        let build = |owner: &str| {
            Workflow::bare()
                .register(S::Start, Probe::new(owner, "single", -1, S::Mid))
                .register(
                    S::Mid,
                    Probe::new(owner, "single", -1, S::Done).failing_once(Arc::clone(&gate)),
                )
                .add_exit_state(S::Done)
                .with_checkpoint_store(Arc::clone(&store) as Arc<dyn CheckpointStore>)
                .with_workflow_id(id.as_str())
        };
        build("")
            .orchestrate(S::Start, CancellationToken::disabled())
            .await
            .expect_err("Mid fails its first call and leaves a checkpoint to resume from");
        let resumed = build(&id);
        let resumed = match user_span {
            Some(span) => resumed.with_tracing_span(span),
            None => resumed,
        };
        resumed
            .resume_from(id.as_str(), CancellationToken::disabled())
            .await
    })
}

/// workflow_resume
fn resume(id: String) -> Run {
    resumed(id, None)
}

/// workflow_resume replaced by the caller's `with_tracing_span` root.
fn resume_user_span(id: String) -> Run {
    let span = tracing::info_span!("user_root", workflow_id = id.as_str());
    resumed(id, Some(span))
}

/// task_attempt per attempt: attempt 1 errors, attempt 2 hangs until `attempt_timeout` drops it,
/// attempt 3 succeeds (6 attempts allowed, so a timeout tripped by a starved CI box only
/// shifts which attempt is the last).
fn retry(id: String) -> Run {
    Box::pin(async move {
        let config = TaskConfig::minimal()
            .with_fixed_retry(5, Duration::from_millis(1))
            .with_attempt_timeout(Duration::from_millis(100));
        Workflow::bare()
            .register(
                S::Start,
                Probe::new(&id, "single", -1, S::Done)
                    .with_config(config)
                    .with_script(&[Act::Fail, Act::Hang]),
            )
            .add_exit_state(S::Done)
            .with_workflow_id(id.as_str())
            .orchestrate(S::Start, CancellationToken::disabled())
            .await
    })
}

/// `JoinStrategy::Any`: two branches hang forever and are aborted once the third finishes; the
/// workflow then carries on into `Mid`.
fn split_any(id: String) -> Run {
    Box::pin(async move {
        let barrier = Arc::new(tokio::sync::Barrier::new(3));
        let branch =
            |i: i64| Probe::new(&id, "split", i, S::Mid).rendezvous_at(Arc::clone(&barrier));
        Workflow::bare()
            .register_split(
                S::Start,
                vec![
                    branch(0),
                    branch(1).with_script(&[Act::Hang]),
                    branch(2).with_script(&[Act::Hang]),
                ],
                JoinConfig::new(JoinStrategy::Any, S::Mid),
            )
            .register(S::Mid, Probe::new(&id, "single", -1, S::Done))
            .add_exit_state(S::Done)
            .with_workflow_id(id.as_str())
            .orchestrate(S::Start, CancellationToken::disabled())
            .await
    })
}

/// `Mid` cancels the run from inside its task and is dropped while suspended; `Start`'s saga step
/// is then rolled back, the drain awaiting `compensate` under no task span.
fn cancelled(id: String) -> Run {
    Box::pin(async move {
        let (handle, token) = CancellationToken::new();
        Workflow::bare()
            .register_with_compensation(
                S::Start,
                CompProbe(Probe::new(&id, "compensatable", -1, S::Mid)),
            )
            .register(
                S::Mid,
                Probe::new(&id, "single", -1, S::Done)
                    .with_script(&[Act::Cancel])
                    .cancelling(handle),
            )
            .add_exit_state(S::Done)
            .with_workflow_id(id.as_str())
            .orchestrate(S::Start, token)
            .await
    })
}

/// A scheduled run nests under `execute_flow`. That span carries no `workflow_id`, so the
/// span-close and ownership checks cannot attribute this scenario to a run; only nesting,
/// attempt fields, `seen` and the bystander apply.
#[cfg(feature = "scheduler")]
fn scheduled(id: String) -> Run {
    Box::pin(async move {
        let workflow = Workflow::bare()
            .register(S::Start, Probe::new(&id, "single", -1, S::Mid))
            .register(S::Mid, Probe::new(&id, "single", -1, S::Done))
            .add_exit_state(S::Done);
        let mut scheduler = Scheduler::new();
        scheduler.manual(&id, workflow, S::Start)?;
        let running = scheduler.start().await?;
        running.trigger(&id).await?;
        let deadline = Instant::now() + Duration::from_secs(30);
        while running.status(&id).await.map(|info| info.status) != Some(Status::Completed) {
            assert!(
                Instant::now() < deadline,
                "{id}: scheduled run never completed"
            );
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        running.stop().await?;
        Ok(S::Done)
    })
}

/// `with_tracing_span`: the caller's span is the root, cano opens no `workflow_orchestrate`.
fn user_span(id: String) -> Run {
    Box::pin(async move {
        split_workflow(&id)
            .with_tracing_span(tracing::info_span!("user_root", workflow_id = id.as_str()))
            .orchestrate(S::Start, CancellationToken::disabled())
            .await
    })
}

/// A span the caller opens around `orchestrate` (the `metrics_tracing_context` example's
/// pattern) stays the ancestor of cano's spans, including inside the spawned split branches.
fn ambient_span(id: String) -> Run {
    let span = tracing::info_span!("api_request", workflow_id = id.as_str());
    Box::pin(
        async move {
            split_workflow(&id)
                .orchestrate(S::Start, CancellationToken::disabled())
                .await
        }
        .instrument(span),
    )
}

#[derive(Clone, Copy)]
enum Outcome {
    Completes,
    /// The run fails with an error of this `category()`.
    Fails(&'static str),
}

struct Scenario {
    name: &'static str,
    /// Spans enclosing each task span, innermost first; `true` = must carry this run's
    /// `workflow_id`.
    roots: &'static [(&'static str, bool)],
    build: fn(String) -> Run,
    outcome: Outcome,
    /// `(kind, branch, attempt, step)` events every workflow must emit at least once.
    seen: &'static [(&'static str, i64, u32, &'static str)],
}

const ORCHESTRATE: &[(&str, bool)] = &[("workflow_orchestrate", true)];

const SINGLE: Scenario = Scenario {
    name: "single",
    roots: ORCHESTRATE,
    build: single,
    outcome: Outcome::Completes,
    seen: &[("single", -1, 1, "done")],
};
const SPLIT: Scenario = Scenario {
    name: "split",
    roots: ORCHESTRATE,
    build: split,
    outcome: Outcome::Completes,
    seen: &[
        ("split", 0, 1, "done"),
        ("split", 1, 1, "done"),
        ("single", -1, 1, "done"),
    ],
};
const COMPENSATABLE: Scenario = Scenario {
    name: "compensatable",
    roots: ORCHESTRATE,
    build: compensatable,
    outcome: Outcome::Completes,
    seen: &[("compensatable", -1, 1, "done")],
};
const RESUME: Scenario = Scenario {
    name: "resume",
    roots: &[("workflow_resume", true)],
    build: resume,
    outcome: Outcome::Completes,
    seen: &[("single", -1, 1, "done")],
};
const RETRY: Scenario = Scenario {
    name: "retry",
    roots: ORCHESTRATE,
    build: retry,
    outcome: Outcome::Completes,
    seen: &[
        ("single", -1, 1, "start"),
        ("single", -1, 2, "dropped"),
        ("single", -1, 3, "start"),
    ],
};
const SPLIT_ANY: Scenario = Scenario {
    name: "split_any",
    roots: ORCHESTRATE,
    build: split_any,
    outcome: Outcome::Completes,
    seen: &[
        ("split", 0, 1, "done"),
        ("split", 1, 1, "dropped"),
        ("split", 2, 1, "dropped"),
        ("single", -1, 1, "done"),
    ],
};
const CANCELLED: Scenario = Scenario {
    name: "cancelled",
    roots: ORCHESTRATE,
    build: cancelled,
    outcome: Outcome::Fails("cancelled"),
    seen: &[
        ("compensatable", -1, 1, "done"),
        ("single", -1, 1, "dropped"),
        ("compensate", -1, 0, "done"),
    ],
};
#[cfg(feature = "scheduler")]
const SCHEDULED: Scenario = Scenario {
    name: "scheduled",
    roots: &[("execute_flow", false)],
    build: scheduled,
    outcome: Outcome::Completes,
    seen: &[("single", -1, 1, "done")],
};
const USER_SPAN: Scenario = Scenario {
    name: "user_span",
    roots: &[("user_root", true)],
    build: user_span,
    outcome: Outcome::Completes,
    seen: &[
        ("split", 0, 1, "done"),
        ("split", 1, 1, "done"),
        ("single", -1, 1, "done"),
    ],
};
const RESUME_USER_SPAN: Scenario = Scenario {
    name: "resume_user_span",
    roots: &[("user_root", true)],
    build: resume_user_span,
    outcome: Outcome::Completes,
    seen: &[("single", -1, 1, "done")],
};
const AMBIENT_SPAN: Scenario = Scenario {
    name: "ambient_span",
    roots: &[("workflow_orchestrate", true), ("api_request", true)],
    build: ambient_span,
    outcome: Outcome::Completes,
    seen: &[
        ("split", 0, 1, "done"),
        ("split", 1, 1, "done"),
        ("single", -1, 1, "done"),
    ],
};

// ---------------------------------------------------------------------------
// Driver + checks
// ---------------------------------------------------------------------------

/// Run `count` workflows of `scenario` concurrently beside a bystander task that never enters a
/// span, then check the invariants. `flavor` tags the run so tests sharing the process-wide
/// capture never mix events; see the module docs for why both flavors are needed.
async fn run(scenario: &Scenario, flavor: &str, count: usize) {
    install_capture();
    let tag = format!("{}-{flavor}", scenario.name);
    let done = Arc::new(AtomicBool::new(false));

    let samples = Arc::new(AtomicUsize::new(0));
    let bystander = {
        let (done, samples) = (Arc::clone(&done), Arc::clone(&samples));
        tokio::spawn(async move {
            let mut leaked = HashSet::new();
            while !done.load(Ordering::SeqCst) {
                samples.fetch_add(1, Ordering::SeqCst);
                if let Some(meta) = tracing::Span::current().metadata() {
                    leaked.insert(meta.name());
                }
                tokio::task::yield_now().await;
            }
            leaked
        })
    };
    // Start the workflows only once the bystander is sampling, so a starved CI box cannot run
    // them to completion before it ever polls.
    while samples.load(Ordering::SeqCst) == 0 {
        tokio::task::yield_now().await;
    }

    let workflows: Vec<_> = (0..count)
        .map(|i| tokio::spawn((scenario.build)(format!("{tag}/{i}"))))
        .collect();
    tokio::time::timeout(Duration::from_secs(60), async {
        for workflow in workflows {
            let result = workflow.await.expect("workflow task must not panic");
            match scenario.outcome {
                Outcome::Completes => {
                    result.expect("workflow completes");
                }
                Outcome::Fails(category) => {
                    let err = result.expect_err("workflow fails");
                    assert_eq!(err.category(), category, "{tag}: {err}");
                }
            }
        }
    })
    .await
    .expect("workflows finish in time");

    // Dropped branches (aborted split tasks) are cleaned up by the runtime shortly after the
    // workflow returns; every span the run opened must then be closed.
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let open: Vec<String> = open_spans()
            .values()
            .filter(|(owner, _)| owner.starts_with(&format!("{tag}/")))
            .map(|(owner, name)| format!("{name} of {owner}"))
            .collect();
        if open.is_empty() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "{tag}: spans never closed after the workflows ended: {open:?}"
        );
        tokio::time::sleep(Duration::from_millis(1)).await;
    }

    done.store(true, Ordering::SeqCst);
    let leaked = bystander.await.expect("bystander task must not panic");
    assert!(
        leaked.is_empty(),
        "{tag}: spans left entered while their workflow was suspended: {leaked:?}"
    );

    check_events(&tag, scenario, count);
}

fn check_events(tag: &str, scenario: &Scenario, count: usize) {
    let events = events();
    let prefix = format!("{tag}/");
    let mine: Vec<&EventRecord> = events
        .iter()
        .filter(|e| e.fields["owner"].starts_with(&prefix))
        .collect();
    let owners: HashSet<&str> = mine.iter().map(|e| e.fields["owner"].as_str()).collect();
    assert_eq!(owners.len(), count, "{tag}: every workflow emits events");

    let roots: Vec<&str> = scenario.roots.iter().map(|(name, _)| *name).collect();
    for e in &mine {
        let owner = &e.fields["owner"];
        let names: Vec<&str> = e.scope.iter().map(|s| s.name).collect();
        let task_span = match e.fields["kind"].as_str() {
            "single" => "single_task_execution",
            "split" => "split_task",
            "compensatable" => "compensatable_task_execution",
            "compensate" => {
                // The rollback drain runs inside the workflow's own spans but outside any task
                // span: a task span here would be a leaked or misplaced span.
                assert!(
                    names.ends_with(&roots[..]),
                    "{tag}: {owner}: compensate event not under {roots:?}: {names:?}"
                );
                for leaked in [
                    "single_task_execution",
                    "split_task",
                    "compensatable_task_execution",
                    "task_attempt",
                ] {
                    assert!(
                        !names.contains(&leaked),
                        "{tag}: {owner}: compensate event sits under {leaked}: {names:?}"
                    );
                }
                let first_root = names.len() - roots.len();
                for (i, (root, tagged)) in scenario.roots.iter().enumerate() {
                    if *tagged {
                        assert_eq!(
                            e.scope[first_root + i].fields.get("workflow_id"),
                            Some(owner),
                            "{tag}: {owner}: compensate event sits under another workflow's {root}"
                        );
                    }
                }
                continue;
            }
            other => panic!("unknown probe kind {other}"),
        };
        let at = |name: &str| names.iter().position(|n| *n == name);
        let (Some(attempt), Some(task)) = (at("task_attempt"), at(task_span)) else {
            panic!("{tag}: {owner}: scope {names:?} lacks task_attempt / {task_span}");
        };
        assert!(
            attempt < task && names[task + 1..] == roots[..],
            "{tag}: {owner}: spans not nested as {roots:?} > {task_span} > .. > task_attempt: {names:?}"
        );
        let unique: HashSet<&str> = names.iter().copied().collect();
        assert_eq!(
            unique.len(),
            names.len(),
            "{tag}: {owner}: repeated span in {names:?}"
        );
        for (i, (root, tagged)) in scenario.roots.iter().enumerate() {
            if *tagged {
                assert_eq!(
                    e.scope[task + 1 + i].fields.get("workflow_id"),
                    Some(owner),
                    "{tag}: {owner}: event sits under another workflow's {root}"
                );
            }
        }
        for field in ["attempt", "max_attempts"] {
            assert_eq!(
                e.scope[attempt].fields.get(field),
                Some(&e.fields[field]),
                "{tag}: {owner}: event sits under a task_attempt with another {field}: {:?}",
                e.scope[attempt].fields
            );
        }
        if task_span == "split_task" {
            assert_eq!(
                e.scope[task].fields.get("task_id").map(String::as_str),
                Some(e.fields["branch"].as_str()),
                "{tag}: {owner}: event sits under another branch's split_task"
            );
        }
    }

    // The scenario must actually have exercised what it claims to.
    for owner in &owners {
        let got: HashSet<(&str, i64, u32, &str)> = mine
            .iter()
            .filter(|e| e.fields["owner"] == *owner)
            .map(|e| {
                (
                    e.fields["kind"].as_str(),
                    e.fields["branch"].parse().expect("branch is a number"),
                    e.fields["attempt"].parse().expect("attempt is a number"),
                    e.fields["step"].as_str(),
                )
            })
            .collect();
        for want in scenario.seen {
            assert!(
                got.contains(want),
                "{tag}: {owner}: never emitted {want:?}; got {got:?}"
            );
        }
    }
}

macro_rules! scenarios {
    ($($(#[$attr:meta])* $ct:ident, $mt:ident => $scenario:expr;)*) => {$(
        $(#[$attr])*
        #[tokio::test]
        async fn $ct() {
            run(&$scenario, "ct", 3).await;
        }
        $(#[$attr])*
        #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn $mt() {
            run(&$scenario, "mt", 24).await;
        }
    )*};
}

scenarios! {
    current_thread_single, multi_thread_single => SINGLE;
    current_thread_split, multi_thread_split => SPLIT;
    current_thread_compensatable, multi_thread_compensatable => COMPENSATABLE;
    current_thread_resume, multi_thread_resume => RESUME;
    current_thread_retry, multi_thread_retry => RETRY;
    current_thread_split_any, multi_thread_split_any => SPLIT_ANY;
    current_thread_cancelled, multi_thread_cancelled => CANCELLED;
    #[cfg(feature = "scheduler")]
    current_thread_scheduled, multi_thread_scheduled => SCHEDULED;
    current_thread_user_span, multi_thread_user_span => USER_SPAN;
    current_thread_resume_user_span, multi_thread_resume_user_span => RESUME_USER_SPAN;
    current_thread_ambient_span, multi_thread_ambient_span => AMBIENT_SPAN;
}
