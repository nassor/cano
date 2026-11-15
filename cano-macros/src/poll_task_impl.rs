//! `#[cano::task::poll]`: the companion `Task::run` drives
//! `::cano::task::poll::run_poll_loop`. Surface forms and the companion shape are
//! described in [`crate::model`].

use proc_macro2::TokenStream;
use quote::quote;

use crate::model::{self, Model, no_assoc};

const MODEL: Model = Model {
    attr: "task::poll",
    trait_ident: "PollTask",
    required: &["poll"],
    optional: &["config", "name", "on_poll_error"],
    assoc: &[],
    res_first: &[("poll", 1)],
    infer_assoc: no_assoc,
    companion_run: Some(
        |state| quote!(::std::boxed::Box::pin(::cano::task::poll::run_poll_loop::<Self, #state>(self, res))),
    ),
};

pub(crate) fn expand(attr: TokenStream, item: TokenStream) -> syn::Result<TokenStream> {
    model::expand(&MODEL, attr, item)
}
