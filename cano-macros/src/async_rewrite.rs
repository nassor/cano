//! Async-fn-in-trait rewriter.
//!
//! Shared by the `task`, `node`, and `resource` attribute macros: rewrites every
//! `async fn` method on a trait definition or impl block so it returns
//! `Pin<Box<dyn Future<Output = ...> + Send + 'async_trait>>`. This is the same
//! shape produced by the `async-trait` crate; the macros are drop-in replacements.

use proc_macro::TokenStream;
use proc_macro2::{Span, TokenStream as TokenStream2};
use quote::quote;
use syn::{
    FnArg, GenericParam, ImplItem, ImplItemFn, ItemImpl, ItemTrait, LifetimeParam, ReturnType,
    Signature, TraitItem, TraitItemFn, Type, WhereClause, WherePredicate,
};

/// Entry point. Tries to parse `item` as a trait definition first, then an impl
/// block; emits a compile error otherwise.
///
/// # Limitations
///
/// - `(?Send)` opt-out is not supported; all futures require `Send`.
/// - Owned `self` receivers (`async fn foo(self)`) are not supported.
pub(crate) fn rewrite(item: TokenStream) -> TokenStream {
    if let Ok(trait_def) = syn::parse::<ItemTrait>(item.clone()) {
        return rewrite_trait(trait_def).into();
    }
    if let Ok(impl_block) = syn::parse::<ItemImpl>(item.clone()) {
        return rewrite_impl(impl_block).into();
    }
    syn::Error::new(
        Span::call_site(),
        "cano async-trait macros: expected a trait or impl block",
    )
    .to_compile_error()
    .into()
}

/// Rewrite a parsed `ItemImpl` directly. Used by callers that have already
/// massaged the impl block (e.g. the boilerplate-filler in `node_impl.rs`)
/// before handing it back for the async rewrite.
#[allow(dead_code)] // wired by node_impl in step 4 of the macro plan
pub(crate) fn rewrite_impl_block(item: ItemImpl) -> TokenStream2 {
    rewrite_impl(item)
}

/// Rewrite a parsed `ItemTrait` directly. Used by callers that have already
/// parsed the trait definition (e.g. `router_task_impl.rs`) before handing
/// it back for the async rewrite.
pub(crate) fn rewrite_trait_def(item: ItemTrait) -> TokenStream2 {
    rewrite_trait(item)
}

// ---------------------------------------------------------------------------
// Trait rewriter
// ---------------------------------------------------------------------------

fn rewrite_trait(mut t: ItemTrait) -> TokenStream2 {
    let items: Vec<TraitItem> = t
        .items
        .into_iter()
        .map(|item| match item {
            TraitItem::Fn(method) => TraitItem::Fn(rewrite_trait_method(method)),
            other => other,
        })
        .collect();
    t.items = items;
    quote! { #t }
}

fn rewrite_trait_method(mut method: TraitItemFn) -> TraitItemFn {
    if method.sig.asyncness.is_none() {
        return method;
    }

    let (new_sig, lifetimes_added) = transform_signature(&method.sig);
    method.sig = new_sig;

    if let Some(body) = method.default.take() {
        let lifetime_bounds = lifetime_bounds_stmts(&lifetimes_added);
        method.default = Some(syn::parse_quote! {
            {
                #lifetime_bounds
                ::std::boxed::Box::pin(async move #body)
            }
        });
    }

    method
}

// ---------------------------------------------------------------------------
// Impl rewriter
// ---------------------------------------------------------------------------

fn rewrite_impl(mut i: ItemImpl) -> TokenStream2 {
    let items: Vec<ImplItem> = i
        .items
        .into_iter()
        .map(|item| match item {
            ImplItem::Fn(method) => ImplItem::Fn(rewrite_impl_method(method)),
            other => other,
        })
        .collect();
    i.items = items;
    quote! { #i }
}

fn rewrite_impl_method(mut method: ImplItemFn) -> ImplItemFn {
    if method.sig.asyncness.is_none() {
        return method;
    }

    let (new_sig, lifetimes_added) = transform_signature(&method.sig);
    method.sig = new_sig;

    let body = &method.block;
    let lifetime_bounds = lifetime_bounds_stmts(&lifetimes_added);
    method.block = syn::parse_quote! {
        {
            #lifetime_bounds
            ::std::boxed::Box::pin(async move #body)
        }
    };

    method
}

// ---------------------------------------------------------------------------
// Core signature transformer
// ---------------------------------------------------------------------------

/// Information about a synthesised lifetime.
struct SynthLifetime {
    name: syn::Lifetime,
}

/// Transform an `async fn` signature into a non-async `fn` returning
/// `Pin<Box<dyn Future<Output = ...> + Send + 'async_trait>>`.
fn transform_signature(sig: &Signature) -> (Signature, Vec<SynthLifetime>) {
    let mut new_sig = sig.clone();
    new_sig.asyncness = None;

    let output_ty: Type = match &sig.output {
        ReturnType::Default => syn::parse_quote! { () },
        ReturnType::Type(_, ty) => *ty.clone(),
    };

    let async_trait_lt: syn::Lifetime = syn::Lifetime::new("'async_trait", Span::call_site());

    let mut synth_lifetimes: Vec<SynthLifetime> = Vec::new();
    let mut new_inputs: syn::punctuated::Punctuated<FnArg, syn::token::Comma> =
        syn::punctuated::Punctuated::new();
    for arg in &sig.inputs {
        new_inputs.push(rewrite_arg(arg, &mut synth_lifetimes));
    }
    new_sig.inputs = new_inputs;

    let has_ref_self = sig.inputs.iter().any(
        |a| matches!(a, FnArg::Receiver(r) if r.reference.is_some() && r.mutability.is_none()),
    );
    let has_mut_self = sig.inputs.iter().any(
        |a| matches!(a, FnArg::Receiver(r) if r.reference.is_some() && r.mutability.is_some()),
    );

    for lt_info in &synth_lifetimes {
        new_sig
            .generics
            .params
            .push(GenericParam::Lifetime(LifetimeParam::new(
                lt_info.name.clone(),
            )));
    }
    new_sig
        .generics
        .params
        .push(GenericParam::Lifetime(LifetimeParam::new(
            async_trait_lt.clone(),
        )));

    let existing_where = new_sig.generics.where_clause.take();
    let mut new_predicates: syn::punctuated::Punctuated<WherePredicate, syn::token::Comma> =
        syn::punctuated::Punctuated::new();
    if let Some(wc) = existing_where {
        for pred in wc.predicates {
            new_predicates.push(pred);
        }
    }

    for lt_info in &synth_lifetimes {
        let lt = &lt_info.name;
        let pred: WherePredicate = syn::parse_quote! { #lt: #async_trait_lt };
        new_predicates.push(pred);
    }
    if has_ref_self {
        let pred: WherePredicate =
            syn::parse_quote! { Self: ::core::marker::Sync + #async_trait_lt };
        new_predicates.push(pred);
    } else if has_mut_self {
        let pred: WherePredicate =
            syn::parse_quote! { Self: ::core::marker::Send + #async_trait_lt };
        new_predicates.push(pred);
    }

    new_sig.generics.where_clause = Some(WhereClause {
        where_token: syn::token::Where {
            span: Span::call_site(),
        },
        predicates: new_predicates,
    });

    new_sig.output = syn::parse_quote! {
        -> ::core::pin::Pin<::std::boxed::Box<
            dyn ::core::future::Future<Output = #output_ty>
                + ::core::marker::Send
                + #async_trait_lt
        >>
    };

    (new_sig, synth_lifetimes)
}

fn rewrite_arg(arg: &FnArg, acc: &mut Vec<SynthLifetime>) -> FnArg {
    match arg {
        FnArg::Receiver(r) => {
            if r.reference.is_some() {
                let lt = make_lifetime(acc);
                let mutability = r.mutability;
                let attrs = r.attrs.clone();
                let self_token = r.self_token;
                let new_receiver: syn::Receiver = syn::parse_quote! {
                    #(#attrs)* &#lt #mutability #self_token
                };
                FnArg::Receiver(new_receiver)
            } else {
                arg.clone()
            }
        }
        FnArg::Typed(pt) => {
            let new_ty = rewrite_type(&pt.ty, acc);
            FnArg::Typed(syn::PatType {
                attrs: pt.attrs.clone(),
                pat: pt.pat.clone(),
                colon_token: pt.colon_token,
                ty: Box::new(new_ty),
            })
        }
    }
}

fn rewrite_type(ty: &Type, acc: &mut Vec<SynthLifetime>) -> Type {
    match ty {
        Type::Reference(r) if r.lifetime.is_none() => {
            let lt = make_lifetime(acc);
            let mutability = r.mutability;
            let inner = rewrite_type(&r.elem, acc);
            syn::parse_quote! { &#lt #mutability #inner }
        }
        other => other.clone(),
    }
}

fn make_lifetime(acc: &mut Vec<SynthLifetime>) -> syn::Lifetime {
    let name = format!("'life{}", acc.len());
    let lt = syn::Lifetime::new(&name, Span::call_site());
    acc.push(SynthLifetime { name: lt.clone() });
    lt
}

/// `async-trait` does not emit any statements inside the body — the
/// `async move { body }` closure captures everything; lifetime constraints live
/// in the where clause only. We replicate this with an empty token stream.
fn lifetime_bounds_stmts(_lifetimes: &[SynthLifetime]) -> TokenStream2 {
    quote! {}
}
