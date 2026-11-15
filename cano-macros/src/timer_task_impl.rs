//! `#[cano::task::timer]`: the companion `Task::run` drives
//! `::cano::task::timer::run_timer` (so the user crate needs no direct `tokio`
//! dependency). Surface forms and the companion shape are described in
//! [`crate::model`].

use proc_macro2::TokenStream;
use quote::quote;

use crate::model::{self, Model, no_assoc};

const MODEL: Model = Model {
    attr: "task::timer",
    trait_ident: "TimerTask",
    required: &["wait", "after_wait"],
    optional: &["config", "name"],
    assoc: &[],
    infer_assoc: no_assoc,
    companion_run: Some(
        |state, key| quote!(::std::boxed::Box::pin(::cano::task::timer::run_timer::<Self, #state, #key>(self, res))),
    ),
};

pub(crate) fn expand(attr: TokenStream, item: TokenStream) -> syn::Result<TokenStream> {
    model::expand(&MODEL, attr, item)
}
