//! Boilerplate-filling pass for `#[cano::checkpoint_store]` on inherent
//! `impl T { ... }` blocks.
//!
//! `#[checkpoint_store] impl T { async fn append(..); async fn load_run(..); async fn clear(..); }`
//! expands to `impl ::cano::CheckpointStore for T { ... }` with every `async fn`
//! rewritten to return `Pin<Box<dyn Future + Send>>`. Unlike `#[task]` /
//! `#[compensatable_task]`, `CheckpointStore` has no type parameters, so there are
//! no attribute args.
//!
//! The explicit `#[checkpoint_store] impl CheckpointStore for T { ... }` form (and
//! `#[checkpoint_store] trait CheckpointStore { ... }`) go through the plain async
//! rewriter in `lib.rs`.

use proc_macro2::TokenStream;
use quote::quote;
use syn::{ImplItem, ImplItemFn, ItemImpl, parse2, spanned::Spanned};

use crate::attr_args::combine_errors;

pub(crate) fn expand(item: TokenStream) -> syn::Result<TokenStream> {
    let item_impl: ItemImpl = parse2(item)?;
    debug_assert!(
        item_impl.trait_.is_none(),
        "caller routes only inherent impls here"
    );

    let mut append: Option<&ImplItemFn> = None;
    let mut load_run: Option<&ImplItemFn> = None;
    let mut clear: Option<&ImplItemFn> = None;
    let mut errors: Vec<syn::Error> = Vec::new();

    for it in &item_impl.items {
        match it {
            ImplItem::Fn(f) => match f.sig.ident.to_string().as_str() {
                "append" => append = Some(f),
                "load_run" => load_run = Some(f),
                "clear" => clear = Some(f),
                other => errors.push(syn::Error::new_spanned(
                    &f.sig.ident,
                    format!(
                        "#[cano::checkpoint_store]: unexpected method `{other}` in inherent impl; \
                         the trait has `append`, `load_run`, and `clear`"
                    ),
                )),
            },
            ImplItem::Type(t) => errors.push(syn::Error::new_spanned(
                &t.ident,
                "#[cano::checkpoint_store]: the `CheckpointStore` trait has no associated types",
            )),
            _ => {}
        }
    }

    for (got, name, sig) in [
        (
            append.is_some(),
            "append",
            "append(&self, workflow_id: &str, row: CheckpointRow) -> Result<(), CanoError>",
        ),
        (
            load_run.is_some(),
            "load_run",
            "load_run(&self, workflow_id: &str) -> Result<Vec<CheckpointRow>, CanoError>",
        ),
        (
            clear.is_some(),
            "clear",
            "clear(&self, workflow_id: &str) -> Result<(), CanoError>",
        ),
    ] {
        if !got {
            errors.push(syn::Error::new(
                item_impl.span(),
                format!("#[cano::checkpoint_store] requires `async fn {sig}` — missing `{name}`"),
            ));
        }
    }
    if !errors.is_empty() {
        return Err(combine_errors(errors));
    }

    let attrs = &item_impl.attrs;
    let unsafety = &item_impl.unsafety;
    let generics = &item_impl.generics;
    let where_clause = &item_impl.generics.where_clause;
    let self_ty = &item_impl.self_ty;
    let user_items = &item_impl.items;

    let synth = quote! {
        #(#attrs)*
        #unsafety impl #generics ::cano::CheckpointStore for #self_ty #where_clause {
            #(#user_items)*
        }
    };

    let synth_impl: ItemImpl = parse2(synth)?;
    Ok(crate::async_rewrite::rewrite_impl_block(synth_impl))
}
