//! Async-fn-in-trait rewriter.
//!
//! Shared by every cano attribute macro (`task`, `resource`, `checkpoint_store`,
//! and the specialized task macros): rewrites every `async fn` method on a trait
//! definition or impl block so it returns
//! `Pin<Box<dyn Future<Output = ...> + Send + 'async_trait>>`. This is the same
//! shape produced by the `async-trait` crate; the macros are drop-in replacements.

use proc_macro::TokenStream;
use proc_macro2::{Span, TokenStream as TokenStream2};
use quote::quote;
use syn::{
    FnArg, GenericParam, ImplItem, ImplItemFn, ItemImpl, ItemTrait, LifetimeParam, ReceiverKind,
    ReturnType, Signature, TraitItem, TraitItemFn, Type, WhereClause, WherePredicate,
};

/// Entry point. Tries to parse `item` as a trait definition first, then an impl
/// block; emits a compile error otherwise.
///
/// # Limitations
///
/// - `(?Send)` opt-out is not supported; all futures require `Send`.
/// - Owned `self` receivers (`async fn foo(self)`) are not supported.
pub(crate) fn rewrite(attr: TokenStream, item: TokenStream, macro_name: &str) -> TokenStream {
    let attr2: TokenStream2 = attr.into();
    if !attr2.is_empty() {
        return syn::Error::new_spanned(
            attr2,
            format!("#[cano::{macro_name}] does not accept attribute arguments"),
        )
        .to_compile_error()
        .into();
    }
    if let Ok(trait_def) = syn::parse::<ItemTrait>(item.clone()) {
        return rewrite_trait(trait_def)
            .unwrap_or_else(syn::Error::into_compile_error)
            .into();
    }
    if let Ok(impl_block) = syn::parse::<ItemImpl>(item) {
        return rewrite_impl(impl_block)
            .unwrap_or_else(syn::Error::into_compile_error)
            .into();
    }
    syn::Error::new(
        Span::call_site(),
        format!("#[cano::{macro_name}]: expected a trait or impl block"),
    )
    .to_compile_error()
    .into()
}

// ---------------------------------------------------------------------------
// Trait rewriter
// ---------------------------------------------------------------------------

pub(crate) fn rewrite_trait(mut t: ItemTrait) -> syn::Result<TokenStream2> {
    let mut items: Vec<TraitItem> = Vec::with_capacity(t.items.len());
    for item in t.items {
        match item {
            TraitItem::Fn(method) => items.push(TraitItem::Fn(rewrite_trait_method(method)?)),
            other => items.push(other),
        }
    }
    t.items = items;
    Ok(quote! { #t })
}

fn rewrite_trait_method(mut method: TraitItemFn) -> syn::Result<TraitItemFn> {
    if method.sig.asyncness.is_none() {
        return Ok(method);
    }

    method.sig = transform_signature(&method.sig)?;

    if let Some(body) = method.default.take() {
        method.default = Some(syn::parse_quote! {
            {
                ::std::boxed::Box::pin(async move #body)
            }
        });
    }

    Ok(method)
}

// ---------------------------------------------------------------------------
// Impl rewriter
// ---------------------------------------------------------------------------

pub(crate) fn rewrite_impl(mut i: ItemImpl) -> syn::Result<TokenStream2> {
    let mut items: Vec<ImplItem> = Vec::with_capacity(i.items.len());
    for item in i.items {
        match item {
            ImplItem::Fn(method) => items.push(ImplItem::Fn(rewrite_impl_method(method)?)),
            other => items.push(other),
        }
    }
    i.items = items;
    Ok(quote! { #i })
}

fn rewrite_impl_method(mut method: ImplItemFn) -> syn::Result<ImplItemFn> {
    if method.sig.asyncness.is_none() {
        return Ok(method);
    }

    method.sig = transform_signature(&method.sig)?;

    let body = &method.block;
    method.block = syn::parse_quote! {
        {
            ::std::boxed::Box::pin(async move #body)
        }
    };

    Ok(method)
}

// ---------------------------------------------------------------------------
// Core signature transformer
// ---------------------------------------------------------------------------

/// Information about synthesised and explicit lifetimes collected from a signature.
#[derive(Default)]
struct LifetimeCollector {
    synth: Vec<syn::Lifetime>,
    explicit: Vec<syn::Lifetime>,
}

impl LifetimeCollector {
    fn next_synth(&mut self) -> syn::Lifetime {
        let name = format!("'life{}", self.synth.len());
        let lt = syn::Lifetime::new(&name, Span::call_site());
        self.synth.push(lt.clone());
        lt
    }

    fn record_explicit(&mut self, lt: &syn::Lifetime) {
        if lt.ident != "_" && !self.explicit.iter().any(|e| e.ident == lt.ident) {
            self.explicit.push(lt.clone());
        }
    }
}

/// Transform an `async fn` signature into a non-async `fn` returning
/// `Pin<Box<dyn Future<Output = ...> + Send + 'async_trait>>`.
fn transform_signature(sig: &Signature) -> syn::Result<Signature> {
    let mut new_sig = sig.clone();
    new_sig.asyncness = None;

    let output_ty: Type = match &sig.output {
        ReturnType::Default => syn::parse_quote! { () },
        ReturnType::Type(_, ty) => *ty.clone(),
    };

    let async_trait_lt: syn::Lifetime = syn::Lifetime::new("'async_trait", Span::call_site());

    let mut collector = LifetimeCollector::default();
    for lp in sig.generics.lifetimes() {
        collector.record_explicit(&lp.lifetime);
    }

    let mut new_inputs: syn::punctuated::Punctuated<FnArg, syn::token::Comma> =
        syn::punctuated::Punctuated::new();
    for arg in &sig.inputs {
        new_inputs.push(rewrite_arg(arg, &mut collector)?);
    }
    new_sig.inputs = new_inputs;

    let has_ref_self = sig.inputs.iter().any(|a| {
        matches!(a, FnArg::Receiver(r) if matches!(r.kind, ReceiverKind::Reference(_, _, None)))
    });
    let has_mut_self = sig.inputs.iter().any(|a| {
        matches!(a, FnArg::Receiver(r) if matches!(r.kind, ReceiverKind::Reference(_, _, Some(_))))
    });

    let existing_where = new_sig.generics.where_clause.take();
    let where_bounded_types: std::collections::HashSet<syn::Ident> = existing_where
        .iter()
        .flat_map(|wc| &wc.predicates)
        .filter_map(|pred| match pred {
            WherePredicate::Type(pt) => match &pt.bounded_ty {
                Type::Path(tp) if tp.qself.is_none() => tp.path.get_ident().cloned(),
                _ => None,
            },
            _ => None,
        })
        .collect();
    let where_bounded_lifetimes: std::collections::HashSet<syn::Ident> = existing_where
        .iter()
        .flat_map(|wc| &wc.predicates)
        .filter_map(|pred| match pred {
            WherePredicate::Lifetime(pl) => Some(pl.lifetime.ident.clone()),
            _ => None,
        })
        .collect();

    // Rust requires lifetime parameters to precede type and const parameters.
    // Attach `'async_trait` inline unless the parameter already has a `where` clause
    // predicate, so `clippy::multiple_bound_locations` never triggers.
    let mut non_lifetimes: Vec<GenericParam> = Vec::new();
    let mut reordered: syn::punctuated::Punctuated<GenericParam, syn::token::Comma> =
        syn::punctuated::Punctuated::new();
    let mut where_lifetimes: Vec<syn::Lifetime> = Vec::new();
    for param in new_sig.generics.params {
        match param {
            GenericParam::Lifetime(mut lp) => {
                if where_bounded_lifetimes.contains(&lp.lifetime.ident) {
                    where_lifetimes.push(lp.lifetime.clone());
                } else {
                    lp.bounds.push(async_trait_lt.clone());
                }
                reordered.push(GenericParam::Lifetime(lp));
            }
            other => non_lifetimes.push(other),
        }
    }
    for lt in &collector.synth {
        let mut lp = LifetimeParam::new(lt.clone());
        lp.bounds.push(async_trait_lt.clone());
        reordered.push(GenericParam::Lifetime(lp));
    }
    reordered.push(GenericParam::Lifetime(LifetimeParam::new(
        async_trait_lt.clone(),
    )));
    let mut where_types: Vec<syn::Ident> = Vec::new();
    for param in non_lifetimes {
        match param {
            GenericParam::Type(mut tp) => {
                if where_bounded_types.contains(&tp.ident) {
                    where_types.push(tp.ident.clone());
                } else {
                    tp.bounds
                        .push(syn::TypeParamBound::Lifetime(async_trait_lt.clone()));
                }
                reordered.push(GenericParam::Type(tp));
            }
            other => reordered.push(other),
        }
    }
    new_sig.generics.params = reordered;

    let mut new_predicates: syn::punctuated::Punctuated<WherePredicate, syn::token::Comma> =
        syn::punctuated::Punctuated::new();
    if let Some(wc) = existing_where {
        for pred in wc.predicates {
            new_predicates.push(pred);
        }
    }

    for lt in &where_lifetimes {
        let pred: WherePredicate = syn::parse_quote! { #lt: #async_trait_lt };
        new_predicates.push(pred);
    }
    for tp_ident in &where_types {
        let pred: WherePredicate = syn::parse_quote! { #tp_ident: #async_trait_lt };
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

    Ok(new_sig)
}

fn rewrite_arg(arg: &FnArg, acc: &mut LifetimeCollector) -> syn::Result<FnArg> {
    match arg {
        FnArg::Receiver(r) => match &r.kind {
            ReceiverKind::Reference(_, existing_lt, mutability) => {
                let lt = match existing_lt {
                    Some(lt) if lt.ident != "_" => {
                        acc.record_explicit(lt);
                        lt.clone()
                    }
                    _ => acc.next_synth(),
                };
                let attrs = r.attrs.clone();
                let self_token = r.self_token;
                let new_receiver: syn::Receiver = syn::parse_quote! {
                    #(#attrs)* &#lt #mutability #self_token
                };
                Ok(FnArg::Receiver(new_receiver))
            }
            _ => Err(syn::Error::new_spanned(
                r,
                "cano async-trait macros: owned `self` receivers are not supported on `async fn`; \
                 use `&self` or `&mut self`",
            )),
        },
        FnArg::Typed(pt) => {
            let new_ty = rewrite_type(&pt.ty, acc);
            Ok(FnArg::Typed(syn::PatType {
                attrs: pt.attrs.clone(),
                pat: pt.pat.clone(),
                colon_token: pt.colon_token,
                ty: Box::new(new_ty),
            }))
        }
    }
}

fn rewrite_type(ty: &Type, acc: &mut LifetimeCollector) -> Type {
    match ty {
        Type::Reference(r) => {
            let lt = match &r.lifetime {
                Some(lt) if lt.ident != "_" => {
                    acc.record_explicit(lt);
                    lt.clone()
                }
                _ => acc.next_synth(),
            };
            let mutability = r.mutability;
            let inner = rewrite_type(&r.elem, acc);
            syn::parse_quote! { &#lt #mutability #inner }
        }
        Type::Path(tp) => {
            let mut tp = tp.clone();
            if let Some(qself) = &mut tp.qself {
                *qself.ty = rewrite_type(&qself.ty, acc);
            }
            for seg in &mut tp.path.segments {
                if let syn::PathArguments::AngleBracketed(args) = &mut seg.arguments {
                    for arg in &mut args.args {
                        match arg {
                            syn::GenericArgument::Type(inner) => {
                                *inner = rewrite_type(inner, acc);
                            }
                            syn::GenericArgument::Lifetime(lt) => {
                                if lt.ident == "_" {
                                    *lt = acc.next_synth();
                                } else {
                                    acc.record_explicit(lt);
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }
            Type::Path(tp)
        }
        Type::Tuple(tup) => {
            let mut tup = tup.clone();
            for elem in &mut tup.elems {
                *elem = rewrite_type(elem, acc);
            }
            Type::Tuple(tup)
        }
        Type::Slice(s) => {
            let mut s = s.clone();
            *s.elem = rewrite_type(&s.elem, acc);
            Type::Slice(s)
        }
        Type::Array(arr) => {
            let mut arr = arr.clone();
            *arr.elem = rewrite_type(&arr.elem, acc);
            Type::Array(arr)
        }
        Type::Paren(p) => {
            let mut p = p.clone();
            *p.elem = rewrite_type(&p.elem, acc);
            Type::Paren(p)
        }
        Type::Group(g) => {
            let mut g = g.clone();
            *g.elem = rewrite_type(&g.elem, acc);
            Type::Group(g)
        }
        other => other.clone(),
    }
}
