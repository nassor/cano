//! `#[cano::task::stream]`: the companion `Task::run` forwards to the provided
//! method `StreamTask::run_in_memory` (the in-memory windowed loop: no cursor
//! persistence, no cancellation). The inherent form infers:
//!
//! - `type Item`   ← the owned third parameter of `process_item` (`item: T` → `T`);
//! - `type Output` / `type Cursor` ← the `Ok` 2-tuple of `process_item`'s
//!   `Result<(Output, Cursor), _>` return.
//!
//! Surface forms and the companion shape are described in [`crate::model`].

use proc_macro2::TokenStream;
use quote::quote;
use syn::{
    FnArg, GenericArgument, ImplItem, ImplItemFn, ItemImpl, PathArguments, ReturnType, Type,
};

use crate::model::{self, Model, find_fn, has_type, trait_path};

const MODEL: Model = Model {
    attr: "task::stream",
    trait_ident: "StreamTask",
    required: &["open", "process_item", "flush_window", "on_close"],
    optional: &["window", "on_item_error", "config", "name"],
    assoc: &["Item", "Output", "Cursor"],
    res_first: &[
        ("open", 2),
        ("process_item", 2),
        ("flush_window", 2),
        ("on_close", 2),
    ],
    infer_assoc,
    companion_run: Some(|state| {
        let stream = trait_path("StreamTask", state);
        quote!(<Self as #stream>::run_in_memory(self, res))
    }),
    entry: "stream",
};

pub(crate) fn expand(attr: TokenStream, item: TokenStream) -> syn::Result<TokenStream> {
    model::expand(&MODEL, attr, item)
}

fn infer_assoc(item: &ItemImpl) -> syn::Result<Vec<ImplItem>> {
    let process = find_fn(item, "process_item").expect("required method checked");
    let mut out = Vec::new();
    if !has_type(item, "Item") {
        let ty = peel_owned_param(process, "process_item")?;
        out.push(syn::parse_quote!(type Item = #ty;));
    }
    let has_output = has_type(item, "Output");
    let has_cursor = has_type(item, "Cursor");
    if !(has_output && has_cursor) {
        let (output, cursor) = peel_result_tuple_return(process, "process_item")?;
        if !has_output {
            out.push(syn::parse_quote!(type Output = #output;));
        }
        if !has_cursor {
            out.push(syn::parse_quote!(type Cursor = #cursor;));
        }
    }
    Ok(out)
}

/// Return the type of the owned third parameter of `process_item`
/// (`&self`, `res`, then `item: T` → `T`). The type is returned verbatim — a
/// reference is rejected rather than peeled (stream items are owned, unlike
/// `BatchTask::process_item`).
fn peel_owned_param(f: &ImplItemFn, fn_name: &str) -> syn::Result<Type> {
    let third = f.sig.inputs.iter().nth(2).ok_or_else(|| {
        syn::Error::new_spanned(
            &f.sig,
            format!(
                "#[cano::task::stream]: `{fn_name}` must have a third parameter \
                 `item: T` (an owned type) from which `type Item` can be inferred"
            ),
        )
    })?;

    let FnArg::Typed(pt) = third else {
        return Err(syn::Error::new_spanned(
            third,
            format!(
                "#[cano::task::stream]: `{fn_name}` third parameter must be a typed argument, not `self`"
            ),
        ));
    };

    if matches!(&*pt.ty, Type::Reference(_)) {
        return Err(syn::Error::new_spanned(
            &pt.ty,
            format!(
                "#[cano::task::stream]: `{fn_name}`'s `item` parameter must be an owned \
                 type (stream items are moved into the task); found a reference"
            ),
        ));
    }

    Ok((*pt.ty).clone())
}

/// Extract `(Output, Cursor)` from the `Ok` 2-tuple of `process_item`'s
/// `Result<(Output, Cursor), _>` return type.
fn peel_result_tuple_return(f: &ImplItemFn, fn_name: &str) -> syn::Result<(Type, Type)> {
    let ret_ty = match &f.sig.output {
        ReturnType::Type(_, t) => &**t,
        ReturnType::Default => {
            return Err(syn::Error::new_spanned(
                &f.sig,
                format!(
                    "#[cano::task::stream]: `{fn_name}` must return \
                     `Result<(Output, Cursor), CanoError>` so `type Output` / `type Cursor` \
                     can be inferred"
                ),
            ));
        }
    };

    let bad_return = || tuple_return_error(fn_name, ret_ty);

    let Type::Path(tp) = ret_ty else {
        return Err(bad_return());
    };
    let last = tp.path.segments.last().ok_or_else(bad_return)?;
    if last.ident != "Result" && last.ident != "CanoResult" {
        return Err(bad_return());
    }
    let PathArguments::AngleBracketed(args) = &last.arguments else {
        return Err(bad_return());
    };
    let ok_ty = args
        .args
        .iter()
        .find_map(|a| match a {
            GenericArgument::Type(t) => Some(t),
            _ => None,
        })
        .ok_or_else(bad_return)?;

    let Type::Tuple(tuple) = ok_ty else {
        return Err(bad_return());
    };
    if tuple.elems.len() != 2 {
        return Err(bad_return());
    }
    let mut it = tuple.elems.iter();
    let output_ty = it.next().unwrap().clone();
    let cursor_ty = it.next().unwrap().clone();
    Ok((output_ty, cursor_ty))
}

fn tuple_return_error(fn_name: &str, ret_ty: &Type) -> syn::Error {
    syn::Error::new_spanned(
        ret_ty,
        format!(
            "#[cano::task::stream]: `{fn_name}` must return \
             `Result<(Output, Cursor), CanoError>` (a 2-tuple `Ok` type) so that \
             `type Output` / `type Cursor` can be inferred; found `{}`. Write explicit \
             `type Output = ..;` / `type Cursor = ..;` lines if the return shape differs.",
            quote! { #ret_ty }
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `process_item` whose `item` parameter is a reference must be rejected:
    /// stream items are moved into the task, so `type Item` cannot be inferred
    /// from `&T`. All four required methods are present so that expansion gets
    /// past required-method validation and actually reaches `peel_owned_param`.
    #[test]
    fn process_item_rejects_reference_item_param() {
        let attr = quote!(state = TestState);
        let item = quote! {
            impl Counter {
                async fn open(
                    &self,
                    _res: &Resources,
                    _cursor: Option<u64>,
                ) -> Result<Pin<Box<dyn Stream<Item = u64> + Send>>, CanoError> {
                    unimplemented!()
                }

                async fn process_item(
                    &self,
                    _res: &Resources,
                    item: &u64,
                ) -> Result<(u64, u64), CanoError> {
                    unimplemented!()
                }

                async fn flush_window(
                    &self,
                    _res: &Resources,
                    _outputs: Vec<u64>,
                ) -> Result<WindowSignal<u64>, CanoError> {
                    unimplemented!()
                }

                async fn on_close(
                    &self,
                    _res: &Resources,
                    _reason: CloseReason,
                ) -> Result<TestState, CanoError> {
                    unimplemented!()
                }
            }
        };

        let err = expand(attr, item).expect_err("`item: &u64` must not be accepted");
        assert!(
            err.to_string().contains("must be an owned type"),
            "unexpected error: {err}"
        );
    }
}
