//! Boilerplate-filling pass for `#[cano::task::node]` on `impl Node<...> for T` blocks
//! and on inherent `impl T { ... }` blocks.
//!
//! Two entry shapes:
//!
//! 1. **Trait-impl form:** `#[node] impl Node<S> for T { ... }` — user
//!    writes the trait header. The macro fills in `type PrepResult`,
//!    `type ExecResult`, and `fn config` (when missing) and async-rewrites.
//!
//! 2. **Inherent-impl form:** `#[node(state = S [, key = K])] impl T { ... }` —
//!    user writes only the inherent block; the macro builds the
//!    `impl Node<S [, K]> for T` header from the attribute args, enforces that
//!    `prep` / `exec` / `post` are present, infers the associated types, and
//!    injects the default `config`. Returns a `compile_error!` per missing
//!    method.
//!
//! After the boilerplate is in place the impl is handed to
//! `async_rewrite::rewrite_impl_block` to complete the `async fn` →
//! `Pin<Box<dyn Future + Send>>` rewrite.

use proc_macro2::TokenStream;
use quote::quote;
use syn::{
    GenericArgument, ImplItem, ImplItemFn, ItemImpl, PathArguments, ReturnType, Type, parse_quote,
    parse2, spanned::Spanned,
};

use crate::async_rewrite;
use crate::attr_args::AttrArgs;

pub(crate) fn expand(attr: TokenStream, item: TokenStream) -> syn::Result<TokenStream> {
    let item_impl: ItemImpl = parse2(item)?;
    let args = AttrArgs::parse(attr)?;

    if item_impl.trait_.is_some() {
        // Trait-impl form: ignore attr args (none expected) and fill missing
        // associated types / config.
        if args.state.is_some() || args.key.is_some() {
            return Err(syn::Error::new(
                item_impl.span(),
                "#[cano::task::node]: `state` / `key` args only apply to inherent `impl T { ... }` \
                 blocks; when writing `impl Node<...> for T` the trait header already \
                 specifies them",
            ));
        }
        return expand_trait_impl(item_impl);
    }

    // Inherent-impl form: build the trait header from attr args.
    let state_ty = args.state.ok_or_else(|| {
        syn::Error::new(
            item_impl.span(),
            "#[cano::task::node] on an inherent `impl T { ... }` block requires `state = T` \
             (e.g. `#[task::node(state = MyState)]`)",
        )
    })?;
    expand_inherent_impl(item_impl, state_ty, args.key)
}

// ---------------------------------------------------------------------------
// Trait-impl path
// ---------------------------------------------------------------------------

fn expand_trait_impl(mut item_impl: ItemImpl) -> syn::Result<TokenStream> {
    let mut has_prep_result = false;
    let mut has_exec_result = false;
    let mut has_config = false;
    let mut prep_fn: Option<&ImplItemFn> = None;
    let mut exec_fn: Option<&ImplItemFn> = None;

    for it in &item_impl.items {
        match it {
            ImplItem::Type(t) => match t.ident.to_string().as_str() {
                "PrepResult" => has_prep_result = true,
                "ExecResult" => has_exec_result = true,
                _ => {}
            },
            ImplItem::Fn(f) => match f.sig.ident.to_string().as_str() {
                "config" => has_config = true,
                "prep" => prep_fn = Some(f),
                "exec" => exec_fn = Some(f),
                _ => {}
            },
            _ => {}
        }
    }

    let mut injected: Vec<ImplItem> = Vec::new();

    if !has_prep_result {
        let prep = prep_fn.ok_or_else(|| {
            syn::Error::new(
                item_impl.span(),
                "#[cano::task::node] requires either an explicit `type PrepResult = ...;` \
                 or a `prep` method whose return type can be inferred",
            )
        })?;
        let inner = peel_result_return(&prep.sig.output, "prep")?;
        injected.push(parse_quote!(type PrepResult = #inner;));
    }

    if !has_exec_result {
        let exec = exec_fn.ok_or_else(|| {
            syn::Error::new(
                item_impl.span(),
                "#[cano::task::node] requires either an explicit `type ExecResult = ...;` \
                 or an `exec` method whose return type can be read",
            )
        })?;
        let inner: Type = match &exec.sig.output {
            ReturnType::Default => parse_quote!(()),
            ReturnType::Type(_, ty) => (**ty).clone(),
        };
        injected.push(parse_quote!(type ExecResult = #inner;));
    }

    if !has_config {
        injected.push(parse_quote! {
            fn config(&self) -> ::cano::TaskConfig {
                ::cano::TaskConfig::default()
            }
        });
    }

    let mut new_items = injected;
    new_items.append(&mut item_impl.items);
    item_impl.items = new_items;

    Ok(async_rewrite::rewrite_impl_block(item_impl))
}

// ---------------------------------------------------------------------------
// Inherent-impl path
// ---------------------------------------------------------------------------

fn expand_inherent_impl(
    item_impl: ItemImpl,
    state_ty: Type,
    key_ty: Option<Type>,
) -> syn::Result<TokenStream> {
    // Catalogue methods + associated types. Reject unrelated items — the
    // inherent block must contain only `Node` trait items so the rewritten
    // trait impl stays well-formed.
    let mut prep_fn: Option<&ImplItemFn> = None;
    let mut exec_fn: Option<&ImplItemFn> = None;
    let mut post_fn: Option<&ImplItemFn> = None;
    let mut config_fn: Option<&ImplItemFn> = None;
    let mut has_prep_result = false;
    let mut has_exec_result = false;
    let mut errors: Vec<syn::Error> = Vec::new();

    for it in &item_impl.items {
        match it {
            ImplItem::Fn(f) => match f.sig.ident.to_string().as_str() {
                "prep" => prep_fn = Some(f),
                "exec" => exec_fn = Some(f),
                "post" => post_fn = Some(f),
                "config" => config_fn = Some(f),
                "run" | "run_with_retries" => {
                    errors.push(syn::Error::new_spanned(
                        &f.sig.ident,
                        "#[cano::task::node]: do not override `run` / `run_with_retries` in the \
                         inherent form; the trait blanket implementation handles them",
                    ));
                }
                other => {
                    errors.push(syn::Error::new_spanned(
                        &f.sig.ident,
                        format!(
                            "#[cano::task::node]: unexpected method `{other}` in inherent impl; \
                             only `prep`, `exec`, `post`, and `config` are allowed"
                        ),
                    ));
                }
            },
            // Allow explicit `type PrepResult = ...;` / `type ExecResult = ...;`
            // overrides — same semantics as the trait-impl form: the user's
            // declaration wins; otherwise the type is inferred from the
            // method's return type.
            ImplItem::Type(t) => match t.ident.to_string().as_str() {
                "PrepResult" => has_prep_result = true,
                "ExecResult" => has_exec_result = true,
                other => {
                    errors.push(syn::Error::new_spanned(
                        &t.ident,
                        format!(
                            "#[cano::task::node]: unexpected associated type `{other}`; only \
                             `PrepResult` and `ExecResult` are recognised"
                        ),
                    ));
                }
            },
            _ => {}
        }
    }

    if prep_fn.is_none() {
        errors.push(syn::Error::new(
            item_impl.span(),
            "#[cano::task::node] requires a `prep` method: \
             `async fn prep(&self, res: &Resources<_>) -> Result<_, CanoError>`",
        ));
    }
    if exec_fn.is_none() {
        errors.push(syn::Error::new(
            item_impl.span(),
            "#[cano::task::node] requires an `exec` method: \
             `async fn exec(&self, prep: Self::PrepResult) -> Self::ExecResult`",
        ));
    }
    if post_fn.is_none() {
        errors.push(syn::Error::new(
            item_impl.span(),
            "#[cano::task::node] requires a `post` method: \
             `async fn post(&self, res: &Resources<_>, exec: Self::ExecResult) \
              -> Result<TState, CanoError>`",
        ));
    }

    if !errors.is_empty() {
        return Err(combine_errors(errors));
    }

    let prep = prep_fn.unwrap();
    let exec = exec_fn.unwrap();

    // Infer the associated types only when the user didn't supply explicit
    // overrides. If they did, the user's `type ...` items already live in
    // `item_impl.items` and will be emitted via `#(#user_items)*` below.
    let prep_result_inj = if has_prep_result {
        None
    } else {
        let inferred = peel_result_return(&prep.sig.output, "prep")?;
        Some(quote!(type PrepResult = #inferred;))
    };
    let exec_result_inj = if has_exec_result {
        None
    } else {
        let inferred: Type = match &exec.sig.output {
            ReturnType::Default => parse_quote!(()),
            ReturnType::Type(_, ty) => (**ty).clone(),
        };
        Some(quote!(type ExecResult = #inferred;))
    };

    // Build the trait reference: `::cano::Node<#state_ty>` or
    // `::cano::Node<#state_ty, #key_ty>` when an explicit key is given.
    let trait_ref: syn::Path = match key_ty {
        Some(k) => parse_quote!(::cano::Node<#state_ty, #k>),
        None => parse_quote!(::cano::Node<#state_ty>),
    };

    // Carry over generics, where-clause, attributes, and unsafety from the
    // inherent impl onto the synthesised trait impl.
    let attrs = &item_impl.attrs;
    let unsafety = &item_impl.unsafety;
    let generics = &item_impl.generics;
    let where_clause = &item_impl.generics.where_clause;
    let self_ty = &item_impl.self_ty;
    let user_items = &item_impl.items;

    // Inject a default `config` only when the user didn't supply one.
    let config_default = if config_fn.is_none() {
        Some(quote! {
            fn config(&self) -> ::cano::TaskConfig {
                ::cano::TaskConfig::default()
            }
        })
    } else {
        None
    };

    // Assemble synthetic trait-impl tokens, then re-parse + run the async
    // rewriter. Re-parsing keeps a single async-rewrite path (operating on a
    // syntactically-valid `ItemImpl`) instead of duplicating the rewrite logic
    // for raw TokenStreams.
    let synth = quote! {
        #(#attrs)*
        #unsafety impl #generics #trait_ref for #self_ty #where_clause {
            #prep_result_inj
            #exec_result_inj
            #config_default
            #(#user_items)*
        }
    };

    let synth_impl: ItemImpl = parse2(synth)?;
    Ok(async_rewrite::rewrite_impl_block(synth_impl))
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn combine_errors(mut errors: Vec<syn::Error>) -> syn::Error {
    let mut iter = errors.drain(..);
    let mut acc = iter.next().expect("combine_errors called with empty vec");
    for e in iter {
        acc.combine(e);
    }
    acc
}

/// Peel `Result<T, _>` or `CanoResult<T>` from a return type, returning `T`.
fn peel_result_return(ret: &ReturnType, fn_name: &str) -> syn::Result<Type> {
    let ty = match ret {
        ReturnType::Default => {
            return Err(syn::Error::new(
                ret.span(),
                format!(
                    "#[cano::task::node]: `{fn_name}` must have an explicit return type \
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
                "#[cano::task::node]: `{fn_name}` return type must be Result<T, _> or CanoResult<T>"
            ),
        ));
    };

    let last = tp.path.segments.last().ok_or_else(|| {
        syn::Error::new_spanned(
            ty,
            format!("#[cano::task::node]: cannot read return type of `{fn_name}`"),
        )
    })?;

    let ident = &last.ident;
    let PathArguments::AngleBracketed(args) = &last.arguments else {
        return Err(syn::Error::new_spanned(
            ty,
            format!(
                "#[cano::task::node]: `{fn_name}` return type must be Result<T, _> or \
                 CanoResult<T>; found `{ident}` without type parameters"
            ),
        ));
    };

    if ident == "Result" {
        let GenericArgument::Type(t) = args.args.iter().next().ok_or_else(|| {
            syn::Error::new_spanned(
                args,
                format!(
                    "#[cano::task::node]: `{fn_name}` Result<...> needs at least one type parameter"
                ),
            )
        })?
        else {
            return Err(syn::Error::new_spanned(
                args,
                format!(
                    "#[cano::task::node]: `{fn_name}` Result<T, _> first parameter must be a type"
                ),
            ));
        };
        Ok(t.clone())
    } else if ident == "CanoResult" {
        let GenericArgument::Type(t) = args.args.iter().next().ok_or_else(|| {
            syn::Error::new_spanned(
                args,
                format!("#[cano::task::node]: `{fn_name}` CanoResult<T> needs a type parameter"),
            )
        })?
        else {
            return Err(syn::Error::new_spanned(
                args,
                format!("#[cano::task::node]: `{fn_name}` CanoResult<T> parameter must be a type"),
            ));
        };
        Ok(t.clone())
    } else {
        Err(syn::Error::new_spanned(
            ty,
            format!(
                "#[cano::task::node]: `{fn_name}` return type must be Result<T, _> or CanoResult<T>; \
                 found `{ident}<...>`"
            ),
        ))
    }
}
