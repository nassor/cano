//! `#[cano::task::router]`: the companion `Task::run` returns the already-boxed
//! `RouterTask::route` future. Surface forms and the companion shape are described
//! in [`crate::model`].

use proc_macro2::TokenStream;
use quote::quote;

use crate::model::{self, Model, no_assoc, trait_path};

const MODEL: Model = Model {
    attr: "task::router",
    trait_ident: "RouterTask",
    required: &["route"],
    optional: &["config", "name"],
    assoc: &[],
    res_first: &[("route", 1)],
    infer_assoc: no_assoc,
    companion_run: Some(|state| {
        let router = trait_path("RouterTask", state);
        quote!(<Self as #router>::route(self, res))
    }),
};

pub(crate) fn expand(attr: TokenStream, item: TokenStream) -> syn::Result<TokenStream> {
    model::expand(&MODEL, attr, item)
}
