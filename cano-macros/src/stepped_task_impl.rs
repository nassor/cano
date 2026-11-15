//! `#[cano::task::stepped]`: the companion `Task::run` drives
//! `::cano::task::stepped::run_stepped`. The inherent form infers `type Cursor` from
//! the `Option<C>` third parameter of `step`. Surface forms and the companion shape
//! are described in [`crate::model`].

use proc_macro2::TokenStream;
use quote::quote;
use syn::{FnArg, GenericArgument, ImplItem, ImplItemFn, ItemImpl, PathArguments, Type};

use crate::model::{self, Model, find_fn, has_type};

const MODEL: Model = Model {
    attr: "task::stepped",
    trait_ident: "SteppedTask",
    required: &["step"],
    optional: &["config", "name"],
    assoc: &["Cursor"],
    res_first: &[("step", 2)],
    infer_assoc,
    companion_run: Some(
        |state| quote!(::std::boxed::Box::pin(::cano::task::stepped::run_stepped::<Self, #state>(self, res))),
    ),
    entry: "stepped",
};

pub(crate) fn expand(attr: TokenStream, item: TokenStream) -> syn::Result<TokenStream> {
    model::expand(&MODEL, attr, item)
}

fn infer_assoc(item: &ItemImpl) -> syn::Result<Vec<ImplItem>> {
    if has_type(item, "Cursor") {
        return Ok(Vec::new());
    }
    let step = find_fn(item, "step").expect("required method checked");
    let ty = peel_option_param(step, "step")?;
    Ok(vec![syn::parse_quote!(type Cursor = #ty;)])
}

/// Extract `T` from the third parameter `cursor: Option<T>` of `step`
/// (`&self`, `res: &Resources`, then `cursor`).
fn peel_option_param(f: &ImplItemFn, fn_name: &str) -> syn::Result<Type> {
    let third = f.sig.inputs.iter().nth(2).ok_or_else(|| {
        syn::Error::new_spanned(
            &f.sig,
            format!(
                "#[cano::task::stepped]: `{fn_name}` must have a third parameter \
                 `cursor: Option<T>` from which `type Cursor` can be inferred"
            ),
        )
    })?;

    let ty = match third {
        FnArg::Typed(pt) => &*pt.ty,
        FnArg::Receiver(_) => {
            return Err(syn::Error::new_spanned(
                third,
                format!(
                    "#[cano::task::stepped]: `{fn_name}` third parameter must be a typed argument, not `self`"
                ),
            ));
        }
    };

    let not_option = || {
        syn::Error::new_spanned(
            ty,
            format!(
                "#[cano::task::stepped]: `{fn_name}` third parameter must be `Option<T>` \
                 so that `type Cursor = T` can be inferred; found `{}`",
                quote! { #ty }
            ),
        )
    };
    let Type::Path(tp) = ty else {
        return Err(not_option());
    };
    let last = tp.path.segments.last().ok_or_else(not_option)?;
    if last.ident != "Option" {
        return Err(not_option());
    }
    let PathArguments::AngleBracketed(args) = &last.arguments else {
        return Err(not_option());
    };
    args.args
        .iter()
        .find_map(|a| match a {
            GenericArgument::Type(t) => Some(t.clone()),
            _ => None,
        })
        .ok_or_else(not_option)
}
