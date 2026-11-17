//! `#[cano::task::suspend]`: no companion `Task` impl (a suspend task parks the run,
//! which only the engine path can do). The inherent form infers `type Input` from
//! the `input` parameter of `resume`. Surface forms are described in [`crate::model`].

use proc_macro2::TokenStream;
use syn::{FnArg, ImplItem, ItemImpl};

use crate::model::{self, Model, find_fn, has_type};

const MODEL: Model = Model {
    attr: "task::suspend",
    trait_ident: "SuspendTask",
    required: &["suspend", "resume"],
    optional: &["config", "name"],
    assoc: &["Input"],
    res_first: &[("suspend", 1), ("resume", 2)],
    infer_assoc,
    companion_run: None,
    entry: "suspend",
};

pub(crate) fn expand(attr: TokenStream, item: TokenStream) -> syn::Result<TokenStream> {
    model::expand(&MODEL, attr, item)
}

/// `type Input = T;` from the parameter `input: T` of `resume` (`&self`,
/// `res: &Resources`, then `input`).
fn infer_assoc(item: &ItemImpl) -> syn::Result<Vec<ImplItem>> {
    if has_type(item, "Input") {
        return Ok(Vec::new());
    }
    let f = find_fn(item, "resume").expect("required method checked");
    match f.sig.inputs.iter().nth(2) {
        Some(FnArg::Typed(pt)) => {
            let ty = &*pt.ty;
            Ok(vec![syn::parse_quote!(type Input = #ty;)])
        }
        _ => Err(syn::Error::new_spanned(
            &f.sig,
            "#[cano::task::suspend]: `resume` must have a parameter `input: T` from which \
             `type Input` can be inferred",
        )),
    }
}
