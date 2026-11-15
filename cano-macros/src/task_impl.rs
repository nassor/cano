//! `#[cano::task]`: the plain [`Task`] model. No companion impl (a `Task` is
//! already a `Task`); `async fn run(&self)` without a resources parameter gets
//! `_res: &::cano::Resources` inserted. Surface forms are described in
//! [`crate::model`].

use proc_macro2::TokenStream;

use crate::model::{self, Model, no_assoc};

const MODEL: Model = Model {
    attr: "task",
    trait_ident: "Task",
    required: &["run"],
    optional: &["config", "name"],
    assoc: &[],
    res_first: &[("run", 1)],
    infer_assoc: no_assoc,
    companion_run: None,
    entry: "single",
};

pub(crate) fn expand(attr: TokenStream, item: TokenStream) -> syn::Result<TokenStream> {
    model::expand(&MODEL, attr, item)
}
