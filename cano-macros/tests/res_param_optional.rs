//! Every task model accepts methods written without the leading `res: &Resources`
//! parameter; the macro inserts it.

use cano::prelude::*;
use futures_util::stream::{self, Stream};
use std::pin::Pin;
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum S {
    Start,
    Done,
}

struct Poller;
#[task::poll(state = S)]
impl Poller {
    async fn poll(&self) -> Result<PollOutcome<S>, CanoError> {
        Ok(PollOutcome::Ready(S::Done))
    }
}

struct Timer;
#[task::timer(state = S)]
impl Timer {
    async fn wait(&self) -> Result<TimerOutcome, CanoError> {
        Ok(TimerOutcome::Duration(Duration::ZERO))
    }
    async fn after_wait(&self) -> Result<S, CanoError> {
        Ok(S::Done)
    }
}

struct Batch;
#[task::batch(state = S)]
impl Batch {
    async fn load(&self) -> Result<Vec<u32>, CanoError> {
        Ok(vec![1, 2])
    }
    async fn process_item(&self, item: &u32) -> Result<u32, CanoError> {
        Ok(item * 2)
    }
    async fn finish(&self, outputs: Vec<Result<u32, CanoError>>) -> Result<S, CanoError> {
        assert_eq!(outputs.len(), 2);
        Ok(S::Done)
    }
}

struct Stepper;
#[task::stepped(state = S)]
impl Stepper {
    async fn step(&self, cursor: Option<u32>) -> Result<StepOutcome<u32, S>, CanoError> {
        Ok(match cursor {
            None => StepOutcome::More(1),
            Some(_) => StepOutcome::Done(S::Done),
        })
    }
}

struct Streamer;
#[task::stream(state = S)]
impl Streamer {
    async fn open(
        &self,
        _cursor: Option<u32>,
    ) -> Result<Pin<Box<dyn Stream<Item = u32> + Send>>, CanoError> {
        Ok(Box::pin(stream::iter(vec![1u32, 2])) as Pin<Box<dyn Stream<Item = u32> + Send>>)
    }
    async fn process_item(&self, item: u32) -> Result<(u32, u32), CanoError> {
        Ok((item, item))
    }
    async fn flush_window(&self, _outputs: Vec<u32>) -> Result<WindowSignal<S>, CanoError> {
        Ok(WindowSignal::Continue)
    }
    async fn on_close(&self, _reason: CloseReason) -> Result<S, CanoError> {
        Ok(S::Done)
    }
}

struct Saga;
#[saga::task(state = S)]
impl Saga {
    type Output = u32;
    async fn run(&self) -> Result<(S, u32), CanoError> {
        Ok((S::Done, 1))
    }
    async fn compensate(&self, _output: u32) -> Result<(), CanoError> {
        Ok(())
    }
}

#[tokio::test]
async fn resource_free_signatures_compile_and_run() {
    let res = Resources::new();
    assert_eq!(Task::run(&Poller, &res).await.unwrap(), S::Done);
    assert_eq!(Task::run(&Timer, &res).await.unwrap(), S::Done);
    assert_eq!(Task::run(&Batch, &res).await.unwrap(), S::Done);
    assert_eq!(Task::run(&Stepper, &res).await.unwrap(), S::Done);
    assert_eq!(Task::run(&Streamer, &res).await.unwrap(), S::Done);
    assert_eq!(
        CompensatableTask::<S>::run(&Saga, &res).await.unwrap(),
        (S::Done, 1)
    );
    let _ = S::Start;
}
