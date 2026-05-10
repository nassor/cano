//! Boilerplate-filling pass for `#[cano::compensatable_task]` on inherent
//! `impl T { ... }` blocks.
//!
//! When the user writes
//! `#[compensatable_task(state = S [, key = K])] impl T { type Output = O; ... }`,
//! this pass:
//!
//! 1. Enforces that `type Output`, `run`, and `compensate` are all present (and
//!    that no stray items sneak in).
//! 2. Builds the `impl CompensatableTask<S [, K]> for T` trait header from the
//!    attribute args, carrying over generics + where-clause from the inherent impl.
//! 3. Hands the synthesised impl to `async_rewrite::rewrite_impl_block` so every
//!    `async fn` becomes a `Pin<Box<dyn Future + Send>>`-returning method.
//!
//! The legacy `#[compensatable_task] impl CompensatableTask<S> for T { ... }`
//! form is unchanged and goes through the plain async rewriter in `lib.rs`.

use proc_macro2::TokenStream;
use quote::quote;
use syn::{ImplItem, ImplItemFn, ImplItemType, ItemImpl, Type, parse2, spanned::Spanned};

use crate::async_rewrite;
use crate::attr_args::AttrArgs;

pub(crate) fn expand(attr: TokenStream, item: TokenStream) -> syn::Result<TokenStream> {
    let item_impl: ItemImpl = parse2(item)?;
    let args = AttrArgs::parse(attr)?;

    if item_impl.trait_.is_some() {
        return Err(syn::Error::new(
            item_impl.span(),
            "#[cano::compensatable_task]: `state` / `key` args only apply to inherent \
             `impl T { ... }` blocks; when writing `impl CompensatableTask<...> for T` the \
             trait header already specifies them",
        ));
    }

    let state_ty = args.state.ok_or_else(|| {
        syn::Error::new(
            item_impl.span(),
            "#[cano::compensatable_task] on an inherent `impl T { ... }` block requires \
             `state = T` (e.g. `#[compensatable_task(state = MyState)]`)",
        )
    })?;
    expand_inherent_impl(item_impl, state_ty, args.key)
}

fn expand_inherent_impl(
    item_impl: ItemImpl,
    state_ty: Type,
    key_ty: Option<Type>,
) -> syn::Result<TokenStream> {
    let mut output_ty: Option<&ImplItemType> = None;
    let mut run_fn: Option<&ImplItemFn> = None;
    let mut compensate_fn: Option<&ImplItemFn> = None;
    let mut errors: Vec<syn::Error> = Vec::new();

    for it in &item_impl.items {
        match it {
            ImplItem::Type(t) if t.ident == "Output" => output_ty = Some(t),
            ImplItem::Type(t) => errors.push(syn::Error::new_spanned(
                &t.ident,
                format!(
                    "#[cano::compensatable_task]: unexpected associated type `{}`; the trait \
                     defines only `Output`",
                    t.ident
                ),
            )),
            ImplItem::Fn(f) => match f.sig.ident.to_string().as_str() {
                "run" => run_fn = Some(f),
                "compensate" => compensate_fn = Some(f),
                "config" | "name" => {} // allowed CompensatableTask methods (have defaults)
                other => errors.push(syn::Error::new_spanned(
                    &f.sig.ident,
                    format!(
                        "#[cano::compensatable_task]: unexpected method `{other}` in inherent \
                         impl; only `run`, `compensate`, `config`, and `name` are allowed"
                    ),
                )),
            },
            _ => {}
        }
    }

    if output_ty.is_none() {
        errors.push(syn::Error::new(
            item_impl.span(),
            "#[cano::compensatable_task] requires `type Output = ...;` (the data `run` produces \
             and `compensate` consumes; must be `Serialize + DeserializeOwned + Send + Sync + 'static`)",
        ));
    }
    if run_fn.is_none() {
        errors.push(syn::Error::new(
            item_impl.span(),
            "#[cano::compensatable_task] requires `async fn run(&self, res: &Resources<_>) \
             -> Result<(TaskResult<_>, Self::Output), CanoError>`",
        ));
    }
    if compensate_fn.is_none() {
        errors.push(syn::Error::new(
            item_impl.span(),
            "#[cano::compensatable_task] requires `async fn compensate(&self, res: &Resources<_>, \
             output: Self::Output) -> Result<(), CanoError>`",
        ));
    }
    if !errors.is_empty() {
        return Err(combine_errors(errors));
    }

    let trait_ref: syn::Path = match key_ty {
        Some(k) => syn::parse_quote!(::cano::CompensatableTask<#state_ty, #k>),
        None => syn::parse_quote!(::cano::CompensatableTask<#state_ty>),
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
