//! # StreamTask — Windowed Stream Consumption
//!
//! Run with: `cargo run --example stream_task`
//!
//! Demonstrates a [`StreamTask`] consuming a synthetic event stream
//! (`futures::stream::iter`) one item at a time, aggregating into **count windows**
//! of 3, flushing each window (the place to commit side effects / offsets), and
//! transitioning to the exit state when the source is exhausted.
//!
//! Each `process_item` returns `(output, cursor)` — the cursor is the resumable
//! position. Registered durably with [`Workflow::register_stream`] + a checkpoint
//! store, the engine would persist each flushed window's cursor so a crashed or
//! cancelled run resumes from the last committed position. (This example keeps it
//! store-free so it runs under default features.)

use cano::prelude::*;
use futures_util::{Stream, stream};
use std::pin::Pin;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Step {
    Consume,
    Done,
}

#[derive(Debug, Clone)]
struct Event {
    offset: u64,
    payload: String,
}

#[derive(Debug)]
struct Processed {
    offset: u64,
    bytes: usize,
}

struct ConsumeEvents;

#[task::stream(state = Step)]
impl ConsumeEvents {
    fn window(&self) -> StreamWindow {
        StreamWindow::Count(3)
    }

    async fn open(
        &self,
        _res: &Resources,
        cursor: Option<u64>,
    ) -> Result<Pin<Box<dyn Stream<Item = Event> + Send>>, CanoError> {
        // On a fresh run `cursor` is `None`; on resume it is the last committed
        // offset, and we would skip everything up to and including it.
        let start = cursor.map(|c| c + 1).unwrap_or(0);
        println!("open       : starting from offset {start}");
        let events: Vec<Event> = (start..10)
            .map(|offset| Event {
                offset,
                payload: format!("payload-{offset}"),
            })
            .collect();
        Ok(Box::pin(stream::iter(events)) as Pin<Box<dyn Stream<Item = Event> + Send>>)
    }

    async fn process_item(
        &self,
        _res: &Resources,
        item: Event,
    ) -> Result<(Processed, u64), CanoError> {
        Ok((
            Processed {
                offset: item.offset,
                bytes: item.payload.len(),
            },
            item.offset, // the cursor reached by consuming this item
        ))
    }

    async fn flush_window(
        &self,
        _res: &Resources,
        outputs: Vec<Processed>,
    ) -> Result<WindowSignal<Step>, CanoError> {
        let offsets: Vec<u64> = outputs.iter().map(|p| p.offset).collect();
        let bytes: usize = outputs.iter().map(|p| p.bytes).sum();
        println!(
            "flush      : window of {} events (offsets {offsets:?}, {bytes} bytes) committed",
            outputs.len()
        );
        Ok(WindowSignal::Continue)
    }

    async fn on_close(
        &self,
        _res: &Resources,
        reason: CloseReason,
    ) -> Result<TaskResult<Step>, CanoError> {
        println!("close      : stream ended ({reason:?}) → Done");
        Ok(TaskResult::Single(Step::Done))
    }
}

#[tokio::main]
async fn main() -> CanoResult<()> {
    println!("=== stream_task example ===");

    let workflow = Workflow::bare()
        .register_stream(Step::Consume, ConsumeEvents)
        .add_exit_state(Step::Done);

    let result = workflow
        .orchestrate(Step::Consume, CancellationToken::disabled())
        .await?;
    assert_eq!(result, Step::Done);
    println!("\ncompleted at {result:?}");
    Ok(())
}
