//! Integration tests for `#[cano::task::suspend]`'s inherent form, which emits
//! `::cano::` paths and so is tested outside the `cano` crate.

use cano::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum S {
    Review,
    Done,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
enum D {
    Approve,
}

struct Review;

/// Both methods omit the resources parameter; `type Input` is inferred from `input: D`.
#[task::suspend(state = S)]
impl Review {
    async fn suspend(&self) -> Result<(), CanoError> {
        Ok(())
    }

    async fn resume(&self, input: D) -> Result<S, CanoError> {
        match input {
            D::Approve => Ok(S::Done),
        }
    }
}

#[test]
fn inherent_form_infers_input_type() {
    let _: <Review as SuspendTask<S>>::Input = D::Approve;
}

#[tokio::test]
async fn inherent_form_inserts_resources_parameter() {
    let res = Resources::new();
    SuspendTask::<S>::suspend(&Review, &res).await.unwrap();
    assert_eq!(
        SuspendTask::<S>::resume(&Review, &res, D::Approve)
            .await
            .unwrap(),
        S::Done
    );
}

/// `register` routes through the emitted `Registrable` impl to the suspend engine path,
/// which `validate` rejects without a checkpoint store.
#[test]
fn registers_as_a_suspend_state() {
    let err = Workflow::bare()
        .register(S::Review, Review)
        .add_exit_state(S::Done)
        .validate()
        .unwrap_err();
    assert!(matches!(err, CanoError::Configuration(_)), "{err:?}");
    assert!(err.to_string().contains("checkpoint store"), "{err}");
}
