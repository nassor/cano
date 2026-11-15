//! `#[cano::saga::task]`: the [`CompensatableTask`] model. No companion impl; the
//! inherent form must spell `type Output` (it cannot be inferred). Surface forms are
//! described in [`crate::model`].

use proc_macro2::TokenStream;
use syn::spanned::Spanned;
use syn::{ImplItem, ItemImpl};

use crate::model::{self, Model, has_type};

const MODEL: Model = Model {
    attr: "saga::task",
    trait_ident: "CompensatableTask",
    required: &["run", "compensate"],
    optional: &["config", "name"],
    assoc: &["Output"],
    res_first: &[("run", 1), ("compensate", 2)],
    infer_assoc,
    companion_run: None,
};

pub(crate) fn expand(attr: TokenStream, item: TokenStream) -> syn::Result<TokenStream> {
    model::expand(&MODEL, attr, item)
}

fn infer_assoc(item: &ItemImpl) -> syn::Result<Vec<ImplItem>> {
    if has_type(item, "Output") {
        return Ok(Vec::new());
    }
    Err(syn::Error::new(
        item.span(),
        "a compensatable-task impl requires `type Output = ...;` (the data `run` produces \
         and `compensate` consumes; must be `Serialize + DeserializeOwned + Send + Sync + 'static`)",
    ))
}
