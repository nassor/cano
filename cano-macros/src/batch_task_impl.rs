//! Boilerplate-filling pass for `#[cano::batch_task]` on `impl BatchTask<S [, K]> for T`
//! blocks and on inherent `impl T { ... }` blocks.
//!
//! ## Why a sibling `impl Task` is emitted
//!
//! A blanket `impl<B: BatchTask<..>> Task<..> for B` would conflict (E0119) with the
//! existing `impl<N: Node<..>> Task<..> for N`. Instead, each use of `#[batch_task]` on
//! an **impl** block synthesises a concrete `impl Task<S [, K]> for T` alongside the
//! `BatchTask` impl, with `Task::run` delegating to
//! `::cano::task::batch::run_batch(self, res).await`. This gives the same
//! ergonomics ("register a `BatchTask` directly with `Workflow::register`") without
//! touching coherence.
//!
//! ## Why `run_batch` is always called (never inlined)
//!
//! Unlike `poll_task_impl.rs`, the fan-out loop body uses `futures_util` which is a direct
//! dependency of the `cano` crate but is NOT available to external callers. Inlining the
//! loop body into generated code would produce unresolved path errors. The free fn
//! `::cano::task::batch::run_batch` already imports `futures_util` internally and is the
//! single source of truth for the fan-out logic. The synthesised `Task::run` always
//! delegates there via a qualified path.
//!
//! ## Path handling
//!
//! - Trait-impl form: derive the `run_batch` module path from the same prefix as the
//!   user's `BatchTask` path. For example, `cano::BatchTask` → `cano::task::batch::run_batch`.
//! - Inherent form: use `::cano::task::batch::run_batch`.
//!
//! ## Surface forms
//!
//! 1. **Trait-definition form:** `#[batch_task] pub trait BatchTask<...> { ... }` —
//!    the macro only async-rewrites the trait.
//!
//! 2. **Trait-impl form:** `#[batch_task] impl BatchTask<S> for T { ... }` —
//!    the macro async-rewrites the `BatchTask` impl AND emits a companion
//!    `impl Task<S> for T` using the same path prefix as the `BatchTask` path.
//!
//! 3. **Inherent-impl form:** `#[batch_task(state = S [, key = K])] impl T { ... }` —
//!    the macro builds the `impl ::cano::BatchTask<S [, K]> for T` header, infers
//!    `type Item` and `type ItemOutput` from method signatures, async-rewrites it,
//!    and emits the companion `impl ::cano::Task<S [, K]> for T`.

use proc_macro2::TokenStream;
use quote::quote;
use syn::{
    AngleBracketedGenericArguments, FnArg, GenericArgument, ImplItem, ImplItemFn, ItemImpl,
    ItemTrait, Path, PathArguments, PathSegment, ReturnType, Type, parse2, spanned::Spanned,
};

use crate::async_rewrite;
use crate::attr_args::{AttrArgs, combine_errors};
use crate::path_prefix::{ModulePrefix, derive_module_prefix};

/// Entry point — dispatches based on what `item` parses to.
pub(crate) fn expand(attr: TokenStream, item: TokenStream) -> syn::Result<TokenStream> {
    if let Ok(trait_def) = parse2::<ItemTrait>(item.clone()) {
        if !attr.is_empty() {
            return Err(syn::Error::new(
                trait_def.span(),
                "#[cano::batch_task]: no attribute args are accepted on a trait definition",
            ));
        }
        let rewritten = async_rewrite::rewrite_trait_def(trait_def);
        return Ok(quote! { #rewritten });
    }

    if let Ok(item_impl) = parse2::<ItemImpl>(item.clone()) {
        let args = AttrArgs::parse(attr)?;

        if item_impl.trait_.is_none() {
            // Inherent-impl form: need `state = S`.
            let state_ty = args.state.ok_or_else(|| {
                syn::Error::new(
                    item_impl.span(),
                    "#[cano::batch_task] on an inherent `impl T { ... }` block requires \
                     `state = T` (e.g. `#[batch_task(state = MyState)]`)",
                )
            })?;
            return expand_inherent_impl(item_impl, state_ty, args.key);
        } else {
            if args.state.is_some() || args.key.is_some() {
                return Err(syn::Error::new(
                    item_impl.span(),
                    "#[cano::batch_task]: `state` / `key` args only apply to inherent \
                     `impl T { ... }` blocks; when writing `impl BatchTask<...> for T` the \
                     trait header already specifies them",
                ));
            }
            return expand_trait_impl(item_impl);
        }
    }

    Err(syn::Error::new(
        proc_macro2::Span::call_site(),
        "#[cano::batch_task]: expected a trait definition or impl block",
    ))
}

// ---------------------------------------------------------------------------
// Trait-impl form: `#[batch_task] impl BatchTask<S [, K]> for T { ... }`
// ---------------------------------------------------------------------------

fn expand_trait_impl(item_impl: ItemImpl) -> syn::Result<TokenStream> {
    let (state_ty, key_ty, task_trait_path, run_batch_path, module_prefix) =
        extract_state_key_paths_and_prefix(&item_impl)?;

    let batch_impl = async_rewrite::rewrite_impl_block(item_impl.clone());

    let task_impl = synthesise_task_impl(
        &item_impl,
        &state_ty,
        key_ty.as_ref(),
        task_trait_path,
        &run_batch_path,
        &module_prefix,
    )?;

    Ok(quote! {
        #batch_impl
        #task_impl
    })
}

// ---------------------------------------------------------------------------
// Inherent-impl form: `#[batch_task(state = S [, key = K])] impl T { ... }`
// ---------------------------------------------------------------------------

fn expand_inherent_impl(
    item_impl: ItemImpl,
    state_ty: Type,
    key_ty: Option<Type>,
) -> syn::Result<TokenStream> {
    let mut process_item_fn: Option<&ImplItemFn> = None;
    let mut errors: Vec<syn::Error> = Vec::new();
    let mut has_item_ty = false;
    let mut has_item_output_ty = false;
    let mut has_concurrency = false;
    let mut has_item_retry = false;
    let mut has_config = false;
    let mut has_name = false;
    let mut has_load = false;
    let mut has_finish = false;

    for it in &item_impl.items {
        match it {
            ImplItem::Fn(f) => match f.sig.ident.to_string().as_str() {
                "process_item" => process_item_fn = Some(f),
                "load" => has_load = true,
                "finish" => has_finish = true,
                "concurrency" => has_concurrency = true,
                "item_retry" => has_item_retry = true,
                "config" => has_config = true,
                "name" => has_name = true,
                other => {
                    errors.push(syn::Error::new_spanned(
                        &f.sig.ident,
                        format!(
                            "#[cano::batch_task]: unexpected method `{other}` in inherent impl; \
                             allowed: `load`, `process_item`, `finish`, `concurrency`, \
                             `item_retry`, `config`, `name`"
                        ),
                    ));
                }
            },
            ImplItem::Type(t) => match t.ident.to_string().as_str() {
                "Item" => has_item_ty = true,
                "ItemOutput" => has_item_output_ty = true,
                other => {
                    errors.push(syn::Error::new_spanned(
                        &t.ident,
                        format!(
                            "#[cano::batch_task]: unexpected associated type `{other}`; \
                             only `Item` and `ItemOutput` are recognised"
                        ),
                    ));
                }
            },
            _ => {}
        }
    }

    if process_item_fn.is_none() {
        errors.push(syn::Error::new(
            item_impl.span(),
            "#[cano::batch_task] requires a \
             `async fn process_item(&self, item: &T) -> Result<U, CanoError>` method",
        ));
    }
    if !has_load {
        errors.push(syn::Error::new(
            item_impl.span(),
            "#[cano::batch_task] requires a \
             `async fn load(&self, res: &Resources<_>) -> Result<Vec<_>, CanoError>` method",
        ));
    }
    if !has_finish {
        errors.push(syn::Error::new(
            item_impl.span(),
            "#[cano::batch_task] requires a \
             `async fn finish(&self, res: &Resources<_>, outputs: Vec<Result<_, CanoError>>) \
              -> Result<TaskResult<_>, CanoError>` method",
        ));
    }

    if !errors.is_empty() {
        return Err(combine_errors(errors));
    }

    let proc = process_item_fn.unwrap();

    // Infer `Item` from the second parameter of `process_item` (i.e. `&T` → `T`).
    let item_inj = if has_item_ty {
        None
    } else {
        let inferred = peel_ref_param(proc, "process_item")?;
        Some(quote!(type Item = #inferred;))
    };

    // Infer `ItemOutput` from the return type of `process_item` (i.e. `Result<T, _>` → `T`).
    let item_output_inj = if has_item_output_ty {
        None
    } else {
        let inferred = peel_result_return(&proc.sig.output, "process_item")?;
        Some(quote!(type ItemOutput = #inferred;))
    };

    let batch_trait_ref: syn::Path = match &key_ty {
        Some(k) => syn::parse_quote!(::cano::BatchTask<#state_ty, #k>),
        None => syn::parse_quote!(::cano::BatchTask<#state_ty>),
    };

    let task_trait_path: syn::Path = match &key_ty {
        Some(k) => syn::parse_quote!(::cano::Task<#state_ty, #k>),
        None => syn::parse_quote!(::cano::Task<#state_ty>),
    };

    let run_batch_path: syn::Path = syn::parse_quote!(::cano::task::batch::run_batch);

    let attrs = &item_impl.attrs;
    let unsafety = &item_impl.unsafety;
    let generics = &item_impl.generics;
    let where_clause = &item_impl.generics.where_clause;
    let self_ty = &item_impl.self_ty;
    let user_items = &item_impl.items;

    // Inject defaults for optional methods that were not provided.
    let concurrency_default = if !has_concurrency {
        Some(quote! {
            fn concurrency(&self) -> usize { 1 }
        })
    } else {
        None
    };

    let item_retry_default = if !has_item_retry {
        Some(quote! {
            fn item_retry(&self) -> ::cano::RetryMode {
                ::cano::RetryMode::None
            }
        })
    } else {
        None
    };

    let config_default = if !has_config {
        Some(quote! {
            fn config(&self) -> ::cano::TaskConfig {
                ::cano::TaskConfig::default()
            }
        })
    } else {
        None
    };

    let name_default = if !has_name {
        Some(quote! {
            fn name(&self) -> ::std::borrow::Cow<'static, str> {
                ::std::borrow::Cow::Borrowed(::std::any::type_name::<Self>())
            }
        })
    } else {
        None
    };

    let synth = quote! {
        #(#attrs)*
        #unsafety impl #generics #batch_trait_ref for #self_ty #where_clause {
            #item_inj
            #item_output_inj
            #concurrency_default
            #item_retry_default
            #config_default
            #name_default
            #(#user_items)*
        }
    };

    let synth_impl: ItemImpl = parse2(synth)?;
    let batch_impl = async_rewrite::rewrite_impl_block(synth_impl.clone());

    let module_prefix = ModulePrefix::Cano;
    let task_impl = synthesise_task_impl(
        &synth_impl,
        &state_ty,
        key_ty.as_ref(),
        task_trait_path,
        &run_batch_path,
        &module_prefix,
    )?;

    Ok(quote! {
        #batch_impl
        #task_impl
    })
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn extract_state_key_paths_and_prefix(
    item_impl: &ItemImpl,
) -> syn::Result<(Type, Option<Type>, Path, Path, ModulePrefix)> {
    let (_, trait_path, _) = item_impl
        .trait_
        .as_ref()
        .ok_or_else(|| syn::Error::new(item_impl.span(), "expected a trait impl block"))?;

    let last_seg = trait_path
        .segments
        .last()
        .ok_or_else(|| syn::Error::new(item_impl.span(), "cannot read trait path segments"))?;

    let args = match &last_seg.arguments {
        syn::PathArguments::AngleBracketed(a) => a,
        _ => {
            return Err(syn::Error::new(
                item_impl.span(),
                "BatchTask impl must have angle-bracketed type arguments \
                 (e.g. `BatchTask<MyState>`)",
            ));
        }
    };

    let type_args: Vec<&Type> = args
        .args
        .iter()
        .filter_map(|a| {
            if let syn::GenericArgument::Type(t) = a {
                Some(t)
            } else {
                None
            }
        })
        .collect();

    if type_args.is_empty() {
        return Err(syn::Error::new(
            item_impl.span(),
            "BatchTask impl requires at least one type argument (the state type)",
        ));
    }

    let state_ty = type_args[0].clone();
    let key_ty = type_args.get(1).map(|t| (*t).clone());

    let module_prefix = derive_module_prefix(trait_path);
    let task_path = derive_task_path_from_batch_path(trait_path, &state_ty, key_ty.as_ref())?;
    let run_batch_path = derive_run_batch_path_from_batch_path(trait_path, &module_prefix)?;

    Ok((state_ty, key_ty, task_path, run_batch_path, module_prefix))
}

fn derive_task_path_from_batch_path(
    batch_path: &Path,
    state_ty: &Type,
    key_ty: Option<&Type>,
) -> syn::Result<Path> {
    let mut task_path = batch_path.clone();

    let angle_args: AngleBracketedGenericArguments = match key_ty {
        Some(k) => syn::parse_quote!(<#state_ty, #k>),
        None => syn::parse_quote!(<#state_ty>),
    };
    let task_args = PathArguments::AngleBracketed(angle_args);

    if let Some(last) = task_path.segments.last_mut() {
        *last = PathSegment {
            ident: syn::Ident::new("Task", last.ident.span()),
            arguments: task_args,
        };
    }

    Ok(task_path)
}

/// Derive the `run_batch` free-fn path from the `BatchTask` path.
///
/// Examples:
/// - `BatchTask` (bare) → `task::batch::run_batch`
/// - `cano::BatchTask` → `cano::task::batch::run_batch`
/// - `::cano::BatchTask` → `::cano::task::batch::run_batch`
fn derive_run_batch_path_from_batch_path(
    batch_path: &Path,
    module_prefix: &ModulePrefix,
) -> syn::Result<Path> {
    let run_batch_path: Path = match module_prefix {
        ModulePrefix::Cano => syn::parse_quote!(::cano::task::batch::run_batch),
        ModulePrefix::Prefixed(prefix) => {
            // prefix already includes the trailing `::` — e.g. `mymod::`
            // We need to append `task::batch::run_batch`.
            let ts: proc_macro2::TokenStream = quote! { #prefix task::batch::run_batch };
            syn::parse2(ts)?
        }
        ModulePrefix::Bare => {
            // Bare: assume the user is inside the `cano` crate itself.
            // Derive the prefix from the BatchTask path excluding the last segment,
            // and replace it with `task::batch::run_batch`.
            let seg_count = batch_path.segments.len();
            if seg_count <= 1 {
                // Just `BatchTask` — we're inside `cano`, so relative path.
                syn::parse_quote!(crate::task::batch::run_batch)
            } else {
                let prefix_segs: syn::punctuated::Punctuated<PathSegment, syn::token::PathSep> =
                    batch_path
                        .segments
                        .iter()
                        .take(seg_count.saturating_sub(1))
                        .cloned()
                        .collect();
                let ts = quote! { #prefix_segs::task::batch::run_batch };
                syn::parse2(ts)?
            }
        }
    };
    Ok(run_batch_path)
}

/// Synthesise `impl Task<S [, K]> for T` whose `Task::run` drives the batch loop,
/// forwarding `config` / `name` to `BatchTask::config` / `BatchTask::name` via UFCS.
///
/// ## Why `Task::run` is NOT generated as `async fn`
///
/// The canonical `async_rewrite` pass wraps the body in
/// `Box::pin(async move { ... })` with a `for<'async_trait>` universal lifetime binder.
/// When the body calls `run_batch(self, res).await`, the compiler must satisfy
/// `for<'0> BatchTask<S, Cow<'0, str>>`, but the impl only provides
/// `BatchTask<S, Cow<'static, str>>` (the default). This produces:
///
/// > "implementation of `BatchTask` is not general enough"
///
/// The fix: write the desugared `fn run` signature by hand and use
/// `Box::pin(run_batch(self, res))` — no extra `async move` wrapper.
/// This produces exactly one future state machine (inside `run_batch`) and keeps
/// `K` monomorphised at the concrete type.
fn synthesise_task_impl(
    batch_impl: &ItemImpl,
    state_ty: &Type,
    key_ty: Option<&Type>,
    task_trait_path: Path,
    run_batch_path: &Path,
    module_prefix: &ModulePrefix,
) -> syn::Result<TokenStream> {
    let attrs = &batch_impl.attrs;
    let generics = &batch_impl.generics;
    let where_clause = &batch_impl.generics.where_clause;
    let self_ty = &batch_impl.self_ty;

    let (_, batch_trait_path, _) = batch_impl.trait_.as_ref().ok_or_else(|| {
        syn::Error::new(batch_impl.span(), "expected a BatchTask trait impl block")
    })?;

    let task_config_ty = module_prefix.qualify("TaskConfig");
    let resources_ty = module_prefix.qualify("Resources");
    let task_result_ty = module_prefix.qualify("TaskResult");
    let cano_error_ty = module_prefix.qualify("CanoError");

    let key_ty_tok: TokenStream = match key_ty {
        Some(k) => quote! { #k },
        None => quote! { ::std::borrow::Cow<'static, str> },
    };

    // Generate the desugared `fn run` directly (not `async fn`), so that the
    // async_rewrite pass is not applied to this method.  The return type matches
    // what the `#[task]` trait expects (Pin<Box<dyn Future + Send + 'async_trait>>).
    // Using `Box::pin(run_batch(self, res))` wraps the future returned by the free fn
    // without introducing an extra `async move` capture that would trigger HRTB.
    let synth = quote! {
        #(#attrs)*
        impl #generics #task_trait_path for #self_ty #where_clause {
            fn config(&self) -> #task_config_ty {
                <Self as #batch_trait_path>::config(self)
            }
            fn name(&self) -> ::std::borrow::Cow<'static, str> {
                <Self as #batch_trait_path>::name(self)
            }
            fn run<'life0, 'life1, 'async_trait>(
                &'life0 self,
                res: &'life1 #resources_ty<#key_ty_tok>,
            ) -> ::core::pin::Pin<::std::boxed::Box<
                dyn ::core::future::Future<
                    Output = ::std::result::Result<#task_result_ty<#state_ty>, #cano_error_ty>
                > + ::core::marker::Send + 'async_trait
            >>
            where
                'life0: 'async_trait,
                'life1: 'async_trait,
                Self: ::core::marker::Sync + 'async_trait,
            {
                ::std::boxed::Box::pin(#run_batch_path(self, res))
            }
        }
    };

    // Do NOT run async_rewrite here — the signature is already desugared.
    parse2(synth).map(|impl_block: ItemImpl| quote! { #impl_block })
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
                "#[cano::batch_task]: `{fn_name}` must have a second parameter \
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
                    "#[cano::batch_task]: `{fn_name}` second parameter must be a typed argument, not `self`"
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
                "#[cano::batch_task]: `{fn_name}` second parameter must be `&T` \
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
                    "#[cano::batch_task]: `{fn_name}` must have an explicit return type \
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
                "#[cano::batch_task]: `{fn_name}` return type must be \
                 Result<T, _> or CanoResult<T>"
            ),
        ));
    };

    let last = tp.path.segments.last().ok_or_else(|| {
        syn::Error::new_spanned(
            ty,
            format!("#[cano::batch_task]: cannot read return type of `{fn_name}`"),
        )
    })?;

    let ident = &last.ident;
    let PathArguments::AngleBracketed(args) = &last.arguments else {
        return Err(syn::Error::new_spanned(
            ty,
            format!(
                "#[cano::batch_task]: `{fn_name}` return type must be Result<T, _> or \
                 CanoResult<T>; found `{ident}` without type parameters"
            ),
        ));
    };

    if ident == "Result" {
        let GenericArgument::Type(t) = args.args.iter().next().ok_or_else(|| {
            syn::Error::new_spanned(
                args,
                format!(
                    "#[cano::batch_task]: `{fn_name}` Result<...> needs at least one \
                     type parameter"
                ),
            )
        })?
        else {
            return Err(syn::Error::new_spanned(
                args,
                format!(
                    "#[cano::batch_task]: `{fn_name}` Result<T, _> first parameter \
                     must be a type"
                ),
            ));
        };
        Ok(t.clone())
    } else if ident == "CanoResult" {
        let GenericArgument::Type(t) = args.args.iter().next().ok_or_else(|| {
            syn::Error::new_spanned(
                args,
                format!("#[cano::batch_task]: `{fn_name}` CanoResult<T> needs a type parameter"),
            )
        })?
        else {
            return Err(syn::Error::new_spanned(
                args,
                format!("#[cano::batch_task]: `{fn_name}` CanoResult<T> parameter must be a type"),
            ));
        };
        Ok(t.clone())
    } else {
        Err(syn::Error::new_spanned(
            ty,
            format!(
                "#[cano::batch_task]: `{fn_name}` return type must be Result<T, _> or \
                 CanoResult<T>; found `{ident}<...>`"
            ),
        ))
    }
}
