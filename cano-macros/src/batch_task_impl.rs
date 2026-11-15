//! `#[cano::task::batch]`: the companion `Task::run` drives
//! `::cano::task::batch::run_batch`. The inherent form infers `type Item` from the
//! `&T` parameter of `process_item` and `type ItemOutput` from its `Result<U, _>`
//! return. Surface forms and the companion shape are described in [`crate::model`].

use proc_macro2::TokenStream;
use quote::quote;
use syn::spanned::Spanned;
use syn::{
    FnArg, GenericArgument, ImplItem, ImplItemFn, ItemImpl, PathArguments, ReturnType, Type,
};

use crate::model::{self, Model, find_fn, has_type};

const MODEL: Model = Model {
    attr: "task::batch",
    trait_ident: "BatchTask",
    required: &["load", "process_item", "finish"],
    optional: &["concurrency", "item_retry", "config", "name"],
    assoc: &["Item", "ItemOutput"],
    infer_assoc,
    companion_run: Some(
        |state, key| quote!(::std::boxed::Box::pin(::cano::task::batch::run_batch::<Self, #state, #key>(self, res))),
    ),
};

pub(crate) fn expand(attr: TokenStream, item: TokenStream) -> syn::Result<TokenStream> {
    model::expand(&MODEL, attr, item)
}

fn infer_assoc(item: &ItemImpl) -> syn::Result<Vec<ImplItem>> {
    let process = find_fn(item, "process_item").expect("required method checked");
    let mut out = Vec::new();
    if !has_type(item, "Item") {
        let ty = peel_ref_param(process, "process_item")?;
        out.push(syn::parse_quote!(type Item = #ty;));
    }
    if !has_type(item, "ItemOutput") {
        let ty = peel_result_return(&process.sig.output, "process_item")?;
        out.push(syn::parse_quote!(type ItemOutput = #ty;));
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Type-inference helpers
// ---------------------------------------------------------------------------

/// Extract `T` from the second parameter `item: &T` (or `item: &Self::T`) of
/// `process_item`. The first parameter is `&self`; the second should be `&Item`.
fn peel_ref_param(f: &ImplItemFn, fn_name: &str) -> syn::Result<Type> {
    // Skip the first input (`&self` / `&mut self` / `self`).
    let second = f.sig.inputs.iter().nth(1).ok_or_else(|| {
        syn::Error::new_spanned(
            &f.sig,
            format!(
                "#[cano::task::batch]: `{fn_name}` must have a second parameter \
                 `item: &T` from which `type Item` can be inferred"
            ),
        )
    })?;

    let ty = match second {
        FnArg::Typed(pt) => &*pt.ty,
        FnArg::Receiver(_) => {
            return Err(syn::Error::new_spanned(
                second,
                format!(
                    "#[cano::task::batch]: `{fn_name}` second parameter must be a typed argument, not `self`"
                ),
            ));
        }
    };

    // Expect `&T` or `& 'lt T`.
    match ty {
        Type::Reference(r) => Ok(*r.elem.clone()),
        other => Err(syn::Error::new_spanned(
            other,
            format!(
                "#[cano::task::batch]: `{fn_name}` second parameter must be `&T` \
                 (a shared reference) so that `type Item = T` can be inferred; \
                 found `{}`",
                quote! { #other }
            ),
        )),
    }
}

/// Peel `Result<T, _>` or `CanoResult<T>` from a return type, returning `T`.
fn peel_result_return(ret: &ReturnType, fn_name: &str) -> syn::Result<Type> {
    let ty = match ret {
        ReturnType::Default => {
            return Err(syn::Error::new(
                ret.span(),
                format!(
                    "#[cano::task::batch]: `{fn_name}` must have an explicit return type \
                     (Result<T, _> or CanoResult<T>)"
                ),
            ));
        }
        ReturnType::Type(_, ty) => &**ty,
    };

    let Type::Path(tp) = ty else {
        return Err(syn::Error::new_spanned(
            ty,
            format!(
                "#[cano::task::batch]: `{fn_name}` return type must be \
                 Result<T, _> or CanoResult<T>"
            ),
        ));
    };

    let last = tp.path.segments.last().ok_or_else(|| {
        syn::Error::new_spanned(
            ty,
            format!("#[cano::task::batch]: cannot read return type of `{fn_name}`"),
        )
    })?;

    let ident = &last.ident;
    let PathArguments::AngleBracketed(args) = &last.arguments else {
        return Err(syn::Error::new_spanned(
            ty,
            format!(
                "#[cano::task::batch]: `{fn_name}` return type must be Result<T, _> or \
                 CanoResult<T>; found `{ident}` without type parameters"
            ),
        ));
    };

    if ident == "Result" {
        let GenericArgument::Type(t) = args.args.iter().next().ok_or_else(|| {
            syn::Error::new_spanned(
                args,
                format!(
                    "#[cano::task::batch]: `{fn_name}` Result<...> needs at least one \
                     type parameter"
                ),
            )
        })?
        else {
            return Err(syn::Error::new_spanned(
                args,
                format!(
                    "#[cano::task::batch]: `{fn_name}` Result<T, _> first parameter \
                     must be a type"
                ),
            ));
        };
        Ok(t.clone())
    } else if ident == "CanoResult" {
        let GenericArgument::Type(t) = args.args.iter().next().ok_or_else(|| {
            syn::Error::new_spanned(
                args,
                format!("#[cano::task::batch]: `{fn_name}` CanoResult<T> needs a type parameter"),
            )
        })?
        else {
            return Err(syn::Error::new_spanned(
                args,
                format!("#[cano::task::batch]: `{fn_name}` CanoResult<T> parameter must be a type"),
            ));
        };
        Ok(t.clone())
    } else {
        Err(syn::Error::new_spanned(
            ty,
            format!(
                "#[cano::task::batch]: `{fn_name}` return type must be Result<T, _> or \
                 CanoResult<T>; found `{ident}<...>`"
            ),
        ))
    }
}
