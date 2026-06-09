//! # StreamTask — Flipping Between Processing Modes Mid-Stream
//!
//! Run with: `cargo run --example stream_task`
//!
//! A more involved [`StreamTask`] demo: one continuous source of temperature
//! readings is consumed by a workflow that **flips between two processing modes**
//! depending on the data, processing readings differently in each:
//!
//! - **`Calm`** — batches readings into windows of 3 and reports the window
//!   *average* (cheap, aggregate processing). If any reading in a window is too hot
//!   (`>= HIGH`), it returns [`WindowSignal::Stop`] to flip the FSM to `Alert`.
//! - **`Alert`** — processes readings *one at a time* (`StreamWindow::Count(1)`),
//!   reacting to each individually. Once a reading cools off (`<= COOL`, hysteresis),
//!   it flips back to `Calm`.
//!
//! The two modes are separate [`StreamTask`]s registered at different FSM states,
//! but they pull from the **same shared source** held in [`Resources`]: each mode's
//! `open()` builds a lazy stream that pops one reading at a time, so flipping modes
//! (which drops the current stream) leaves the unconsumed readings in place for the
//! next mode to continue from. The run ends when the source is exhausted.
//!
//! ```text
//!   Calm ⇄ Alert ⇄ Calm … ──(source exhausted)──► Done
//! ```
//!
//! Registering these with [`Workflow::register_stream`] + a checkpoint store would
//! additionally persist each window's cursor for crash-resume (omitted here to keep
//! the example focused on mode-flipping and to run under default features).

use cano::prelude::*;
use futures_util::{Stream, stream};
use std::collections::VecDeque;
use std::pin::Pin;
use std::sync::Mutex;

/// Flip to `Alert` when a calm window contains a reading at or above this.
const HIGH: i32 = 80;
/// Flip back to `Calm` when an alert reading drops to or below this (hysteresis).
const COOL: i32 = 70;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Mode {
    Calm,
    Alert,
    Done,
}

#[derive(Debug, Clone)]
struct Reading {
    seq: u64,
    temp: i32,
}

/// The shared event source — both modes pop from this same queue, so consumption
/// continues seamlessly across mode flips.
struct Source {
    queue: Mutex<VecDeque<Reading>>,
}

#[resource]
impl Resource for Source {}

/// Build a lazy stream that pops one reading at a time from the shared `Source`.
/// Dropping this stream (when the FSM flips modes) leaves the rest in the queue.
fn open_source(source: std::sync::Arc<Source>) -> Pin<Box<dyn Stream<Item = Reading> + Send>> {
    Box::pin(stream::unfold(source, |source| async move {
        let next = source.queue.lock().unwrap().pop_front();
        next.map(|reading| (reading, source))
    }))
}

// ---------------------------------------------------------------------------
// Calm mode — windowed averaging; flips to Alert on a hot window.
// ---------------------------------------------------------------------------

struct CalmMode;

#[task::stream(state = Mode)]
impl CalmMode {
    fn window(&self) -> StreamWindow {
        StreamWindow::Count(3)
    }

    async fn open(
        &self,
        res: &Resources,
        _cursor: Option<u64>,
    ) -> Result<Pin<Box<dyn Stream<Item = Reading> + Send>>, CanoError> {
        Ok(open_source(res.get::<Source, _>("source")?))
    }

    async fn process_item(&self, _res: &Resources, item: Reading) -> Result<(i32, u64), CanoError> {
        Ok((item.temp, item.seq))
    }

    async fn flush_window(
        &self,
        _res: &Resources,
        temps: Vec<i32>,
    ) -> Result<WindowSignal<Mode>, CanoError> {
        let avg = temps.iter().sum::<i32>() as f64 / temps.len() as f64;
        let hottest = *temps.iter().max().unwrap();
        println!("calm  : window {temps:?} → avg {avg:.1}°");
        if hottest >= HIGH {
            println!("calm  : {hottest}° ≥ {HIGH}° — flipping to ALERT");
            return Ok(WindowSignal::Stop(TaskResult::Single(Mode::Alert)));
        }
        Ok(WindowSignal::Continue)
    }

    async fn on_close(
        &self,
        _res: &Resources,
        reason: CloseReason,
    ) -> Result<TaskResult<Mode>, CanoError> {
        println!("calm  : source closed ({reason:?}) → Done");
        Ok(TaskResult::Single(Mode::Done))
    }
}

// ---------------------------------------------------------------------------
// Alert mode — per-reading handling; flips back to Calm once it cools off.
// ---------------------------------------------------------------------------

struct AlertMode;

#[task::stream(state = Mode)]
impl AlertMode {
    fn window(&self) -> StreamWindow {
        // One reading per window — react to each individually.
        StreamWindow::Count(1)
    }

    async fn open(
        &self,
        res: &Resources,
        _cursor: Option<u64>,
    ) -> Result<Pin<Box<dyn Stream<Item = Reading> + Send>>, CanoError> {
        Ok(open_source(res.get::<Source, _>("source")?))
    }

    async fn process_item(&self, _res: &Resources, item: Reading) -> Result<(i32, u64), CanoError> {
        Ok((item.temp, item.seq))
    }

    async fn flush_window(
        &self,
        _res: &Resources,
        temps: Vec<i32>,
    ) -> Result<WindowSignal<Mode>, CanoError> {
        let temp = temps[0];
        let over = temp - HIGH;
        println!("alert : reading {temp}° ({over:+}° vs HIGH) — handling individually");
        if temp <= COOL {
            println!("alert : {temp}° ≤ {COOL}° — cooled off, flipping back to CALM");
            return Ok(WindowSignal::Stop(TaskResult::Single(Mode::Calm)));
        }
        Ok(WindowSignal::Continue)
    }

    async fn on_close(
        &self,
        _res: &Resources,
        reason: CloseReason,
    ) -> Result<TaskResult<Mode>, CanoError> {
        println!("alert : source closed ({reason:?}) → Done");
        Ok(TaskResult::Single(Mode::Done))
    }
}

#[tokio::main]
async fn main() -> CanoResult<()> {
    println!("=== stream_task example: flipping between processing modes ===\n");

    // A synthetic temperature stream that heats up and cools off twice.
    let temps = [70, 71, 73, 85, 88, 76, 71, 90, 68, 73, 74, 95, 88, 60];
    let readings: VecDeque<Reading> = temps
        .iter()
        .enumerate()
        .map(|(i, &temp)| Reading {
            seq: i as u64,
            temp,
        })
        .collect();

    let resources = Resources::new().insert(
        "source",
        Source {
            queue: Mutex::new(readings),
        },
    );

    let workflow = Workflow::new(resources)
        .register_stream(Mode::Calm, CalmMode)
        .register_stream(Mode::Alert, AlertMode)
        .add_exit_state(Mode::Done);

    let result = workflow
        .orchestrate(Mode::Calm, CancellationToken::disabled())
        .await?;
    assert_eq!(result, Mode::Done);
    println!("\ncompleted at {result:?}");
    Ok(())
}
