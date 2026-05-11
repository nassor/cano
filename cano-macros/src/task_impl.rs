//! Boilerplate-filling pass for `#[cano::task]` on inherent `impl T { ... }` blocks.
//!
//! When the user writes `#[task(state = S [, key = K])] impl T { ... }`, this
//! pass:
//!
//! 1. Enforces that **exactly one** of `run` / `run_bare` is implemented (both
//!    or neither is a compile error — that's the headline win over the runtime
//!    `CanoError::Configuration` trap).
//! 2. Builds the `impl Task<S [, K]> for T` trait header from the attribute
//!    args and carries over generics + where-clause from the inherent impl.
//! 3. Hands the synthesised impl to `async_rewrite::rewrite_impl_block` so
//!    every `async fn` becomes a `Pin<Box<dyn Future + Send>>`-returning
//!    method.
//!
//! The legacy `#[task] impl Task<S> for T { ... }` form is unchanged and goes
//! through the plain async rewriter in `lib.rs`.

use proc_macro2::TokenStream;
use quote::quote;
use syn::{ImplItem, ImplItemFn, ItemImpl, Type, parse2, spanned::Spanned};

use crate::async_rewrite;
use crate::attr_args::AttrArgs;

pub(crate) fn expand(attr: TokenStream, item: TokenStream) -> syn::Result<TokenStream> {
    let item_impl: ItemImpl = parse2(item)?;
    let args = AttrArgs::parse(attr)?;

    if item_impl.trait_.is_some() {
        return Err(syn::Error::new(
            item_impl.span(),
            "#[cano::task]: `state` / `key` args only apply to inherent \
             `impl T { ... }` blocks; when writing `impl Task<...> for T` the trait header \
             already specifies them",
        ));
    }

    let state_ty = args.state.ok_or_else(|| {
        syn::Error::new(
            item_impl.span(),
            "#[cano::task] on an inherent `impl T { ... }` block requires `state = T` \
             (e.g. `#[task(state = MyState)]`)",
        )
    })?;

    expand_inherent_impl(item_impl, state_ty, args.key)
}

fn expand_inherent_impl(
    item_impl: ItemImpl,
    state_ty: Type,
    key_ty: Option<Type>,
) -> syn::Result<TokenStream> {
    let mut run_fn: Option<&ImplItemFn> = None;
    let mut run_bare_fn: Option<&ImplItemFn> = None;
    let mut errors: Vec<syn::Error> = Vec::new();

    for it in &item_impl.items {
        match it {
            ImplItem::Fn(f) => match f.sig.ident.to_string().as_str() {
                "run" => run_fn = Some(f),
                "run_bare" => run_bare_fn = Some(f),
                "config" | "name" => {} // allowed Task trait methods
                other => {
                    errors.push(syn::Error::new_spanned(
                        &f.sig.ident,
                        format!(
                            "#[cano::task]: unexpected method `{other}` in inherent impl; \
                             only `run`, `run_bare`, `config`, and `name` are allowed"
                        ),
                    ));
                }
            },
            ImplItem::Type(t) => {
                errors.push(syn::Error::new_spanned(
                    &t.ident,
                    "#[cano::task]: associated types are not part of the `Task` trait",
                ));
            }
            _ => {}
        }
    }

    match (run_fn.is_some(), run_bare_fn.is_some()) {
        (false, false) => errors.push(syn::Error::new(
            item_impl.span(),
            "#[cano::task] requires exactly one of `run` / `run_bare`. \
             Implement `run(&self, res: &Resources<_>)` when the task uses resources, \
             or `run_bare(&self)` when it does not",
        )),
        (true, true) => errors.push(syn::Error::new(
            item_impl.span(),
            "#[cano::task]: implement only one of `run` / `run_bare`, not both. \
             The trait's default `run` already delegates to `run_bare`",
        )),
        _ => {}
    }

    if !errors.is_empty() {
        return Err(combine_errors(errors));
    }

    let trait_ref: syn::Path = match key_ty {
        Some(k) => syn::parse_quote!(::cano::Task<#state_ty, #k>),
        None => syn::parse_quote!(::cano::Task<#state_ty>),
    };

    let attrs = &item_impl.attrs;
    let unsafety = &item_impl.unsafety;
    let generics = &item_impl.generics;
    let where_clause = &item_impl.generics.where_clause;
    let self_ty = &item_impl.self_ty;
    let user_items = &item_impl.items;

    let synth = quote! {
        #(#attrs)*
        #unsafety impl #generics #trait_ref for #self_ty #where_clause {
            #(#user_items)*
        }
    };

    let synth_impl: ItemImpl = parse2(synth)?;
    Ok(async_rewrite::rewrite_impl_block(synth_impl))
}

fn combine_errors(mut errors: Vec<syn::Error>) -> syn::Error {
    let mut iter = errors.drain(..);
    let mut acc = iter.next().expect("combine_errors called with empty vec");
    for e in iter {
        acc.combine(e);
    }
    acc
}
