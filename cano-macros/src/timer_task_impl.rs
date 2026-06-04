//! Boilerplate-filling pass for `#[cano::task::timer]` on `impl TimerTask<S [, K]> for T`
//! blocks and on inherent `impl T { ... }` blocks.
//!
//! ## Why a sibling `impl Task` is emitted
//!
//! A blanket `impl<T: TimerTask<..>> Task<..> for T` would conflict (E0119) with the
//! analogous blanket impls for the other specialized task traits (a type can implement
//! more than one). Instead, each use of `#[timer_task]` on an **impl** block synthesises a
//! concrete `impl Task<S [, K]> for T` alongside the `TimerTask` impl, with `Task::run`
//! delegating to `::cano::task::timer::run_timer(self, res).await`. This gives the same
//! ergonomics ("register a `TimerTask` directly with `Workflow::register`") without
//! touching coherence.
//!
//! ## Path handling
//!
//! Same strategy as `poll_task_impl.rs`:
//! - Trait-impl form: derive type paths from the same prefix as the user's `TimerTask` path.
//! - Inherent form: use `::cano::` paths (valid for external callers).
//!
//! ## Surface forms
//!
//! 1. **Trait-definition form:** `#[timer_task] pub trait TimerTask<...> { ... }` —
//!    the macro only async-rewrites the trait.
//!
//! 2. **Trait-impl form:** `#[timer_task] impl TimerTask<S> for T { async fn wait(...);
//!    async fn after_wait(...) }` — the macro async-rewrites the `TimerTask` impl AND emits a
//!    companion `impl Task<S> for T` using the same path prefix as the `TimerTask` path.
//!
//! 3. **Inherent-impl form:** `#[timer_task(state = S [, key = K])] impl T { async fn wait(...);
//!    async fn after_wait(...) }` — the macro builds the `impl ::cano::TimerTask<S [, K]> for T`
//!    header, async-rewrites it, and emits the companion `impl ::cano::Task<S [, K]> for T`.

use proc_macro2::TokenStream;
use quote::quote;
use syn::{
    AngleBracketedGenericArguments, ImplItem, ImplItemFn, ItemImpl, ItemTrait, Path, PathArguments,
    PathSegment, Type, parse2, spanned::Spanned,
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
                "#[cano::task::timer]: no attribute args are accepted on a trait definition",
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
                    "#[cano::task::timer] on an inherent `impl T { ... }` block requires \
                     `state = T` (e.g. `#[task::timer(state = MyState)]`)",
                )
            })?;
            return expand_inherent_impl(item_impl, state_ty, args.key);
        } else {
            if args.state.is_some() || args.key.is_some() {
                return Err(syn::Error::new(
                    item_impl.span(),
                    "#[cano::task::timer]: `state` / `key` args only apply to inherent \
                     `impl T { ... }` blocks; when writing `impl TimerTask<...> for T` the \
                     trait header already specifies them",
                ));
            }
            return expand_trait_impl(item_impl);
        }
    }

    Err(syn::Error::new(
        proc_macro2::Span::call_site(),
        "#[cano::task::timer]: expected a trait definition or impl block",
    ))
}

// ---------------------------------------------------------------------------
// Trait-impl form: `#[timer_task] impl TimerTask<S [, K]> for T { ... }`
// ---------------------------------------------------------------------------

fn expand_trait_impl(item_impl: ItemImpl) -> syn::Result<TokenStream> {
    let (state_ty, key_ty, task_trait_path, module_prefix) =
        extract_state_key_task_path_and_prefix(&item_impl)?;

    let timer_impl = async_rewrite::rewrite_impl_block(item_impl.clone());

    let task_impl = synthesise_task_impl(
        &item_impl,
        &state_ty,
        key_ty.as_ref(),
        task_trait_path,
        &module_prefix,
    )?;

    Ok(quote! {
        #timer_impl
        #task_impl
    })
}

// ---------------------------------------------------------------------------
// Inherent-impl form: `#[timer_task(state = S [, key = K])] impl T { ... }`
// ---------------------------------------------------------------------------

fn expand_inherent_impl(
    item_impl: ItemImpl,
    state_ty: Type,
    key_ty: Option<Type>,
) -> syn::Result<TokenStream> {
    let mut wait_fn: Option<&ImplItemFn> = None;
    let mut after_wait_fn: Option<&ImplItemFn> = None;
    let mut errors: Vec<syn::Error> = Vec::new();
    let mut has_config = false;
    let mut has_name = false;

    for it in &item_impl.items {
        match it {
            ImplItem::Fn(f) => match f.sig.ident.to_string().as_str() {
                "wait" => wait_fn = Some(f),
                "after_wait" => after_wait_fn = Some(f),
                "config" => has_config = true,
                "name" => has_name = true,
                other => {
                    errors.push(syn::Error::new_spanned(
                        &f.sig.ident,
                        format!(
                            "#[cano::task::timer]: unexpected method `{other}` in inherent impl; \
                             only `wait`, `after_wait`, `config`, and `name` are allowed"
                        ),
                    ));
                }
            },
            ImplItem::Type(t) => {
                errors.push(syn::Error::new_spanned(
                    &t.ident,
                    format!(
                        "#[cano::task::timer]: unexpected associated type `{}`; \
                         `TimerTask` has no associated types",
                        t.ident
                    ),
                ));
            }
            _ => {}
        }
    }

    if wait_fn.is_none() {
        errors.push(syn::Error::new(
            item_impl.span(),
            "#[cano::task::timer] requires an `async fn wait(&self, res: &Resources<_>) \
             -> Result<TimerOutcome, CanoError>` method",
        ));
    }
    if after_wait_fn.is_none() {
        errors.push(syn::Error::new(
            item_impl.span(),
            "#[cano::task::timer] requires an `async fn after_wait(&self, res: &Resources<_>) \
             -> Result<TaskResult<_>, CanoError>` method",
        ));
    }

    if !errors.is_empty() {
        return Err(combine_errors(errors));
    }

    let timer_trait_ref: syn::Path = match &key_ty {
        Some(k) => syn::parse_quote!(::cano::TimerTask<#state_ty, #k>),
        None => syn::parse_quote!(::cano::TimerTask<#state_ty>),
    };

    let task_trait_path: syn::Path = match &key_ty {
        Some(k) => syn::parse_quote!(::cano::Task<#state_ty, #k>),
        None => syn::parse_quote!(::cano::Task<#state_ty>),
    };

    let attrs = &item_impl.attrs;
    let unsafety = &item_impl.unsafety;
    let generics = &item_impl.generics;
    let where_clause = &item_impl.generics.where_clause;
    let self_ty = &item_impl.self_ty;
    let user_items = &item_impl.items;

    // TimerTask defaults to TaskConfig::minimal() (no retries).
    let config_default = if !has_config {
        Some(quote! {
            fn config(&self) -> ::cano::TaskConfig {
                ::cano::TaskConfig::minimal()
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
        #unsafety impl #generics #timer_trait_ref for #self_ty #where_clause {
            #config_default
            #name_default
            #(#user_items)*
        }
    };

    let synth_impl: ItemImpl = parse2(synth)?;
    let timer_impl = async_rewrite::rewrite_impl_block(synth_impl.clone());

    let module_prefix = ModulePrefix::Cano;
    let task_impl = synthesise_task_impl(
        &synth_impl,
        &state_ty,
        key_ty.as_ref(),
        task_trait_path,
        &module_prefix,
    )?;

    Ok(quote! {
        #timer_impl
        #task_impl
    })
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn extract_state_key_task_path_and_prefix(
    item_impl: &ItemImpl,
) -> syn::Result<(Type, Option<Type>, Path, ModulePrefix)> {
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
                "TimerTask impl must have angle-bracketed type arguments (e.g. `TimerTask<MyState>`)",
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
            "TimerTask impl requires at least one type argument (the state type)",
        ));
    }

    let state_ty = type_args[0].clone();
    let key_ty = type_args.get(1).map(|t| (*t).clone());

    let module_prefix = derive_module_prefix(trait_path);
    let task_path = derive_task_path_from_timer_path(trait_path, &state_ty, key_ty.as_ref())?;

    Ok((state_ty, key_ty, task_path, module_prefix))
}

fn derive_task_path_from_timer_path(
    timer_path: &Path,
    state_ty: &Type,
    key_ty: Option<&Type>,
) -> syn::Result<Path> {
    let mut task_path = timer_path.clone();

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

/// Synthesise `impl Task<S [, K]> for T` whose `Task::run` waits then transitions,
/// forwarding `config`/`name` to `TimerTask::config`/`TimerTask::name` via UFCS.
///
/// For the inherent form (and other cases where `::cano::` is valid), `Task::run`
/// delegates to `::cano::task::timer::run_timer(self, res).await`. For the
/// trait-impl form used inside the `cano` crate itself (where `::cano` doesn't
/// exist), the wait/after_wait sequence is inlined directly so no external crate
/// reference is needed.
fn synthesise_task_impl(
    timer_impl: &ItemImpl,
    state_ty: &Type,
    key_ty: Option<&Type>,
    task_trait_path: Path,
    module_prefix: &ModulePrefix,
) -> syn::Result<TokenStream> {
    let attrs = &timer_impl.attrs;
    let generics = &timer_impl.generics;
    let where_clause = &timer_impl.generics.where_clause;
    let self_ty = &timer_impl.self_ty;

    let (_, timer_trait_path, _) = timer_impl.trait_.as_ref().ok_or_else(|| {
        syn::Error::new(timer_impl.span(), "expected a TimerTask trait impl block")
    })?;

    let task_config_ty = module_prefix.qualify("TaskConfig");
    let resources_ty = module_prefix.qualify("Resources");
    let task_result_ty = module_prefix.qualify("TaskResult");
    let cano_error_ty = module_prefix.qualify("CanoError");
    let timer_outcome_ty = module_prefix.qualify("TimerOutcome");

    let key_ty_tok: TokenStream = match key_ty {
        Some(k) => quote! { #k },
        None => quote! { ::std::borrow::Cow<'static, str> },
    };

    // For the inherent form (ModulePrefix::Cano), delegate to the free helper function
    // in cano::task::timer so that the sleep body lives in one place.
    //
    // For the trait-impl form (Bare / Prefixed), inline the body so it compiles both
    // inside the `cano` crate (where `::cano` does not resolve) and in external crates.
    let run_body = match module_prefix {
        ModulePrefix::Cano => quote! {
            ::cano::task::timer::run_timer(self, res).await
        },
        _ => quote! {
            {
                match <Self as #timer_trait_path>::wait(self, res).await? {
                    #timer_outcome_ty::Duration(__d) => {
                        ::tokio::time::sleep(__d).await;
                    }
                    // `sleep_until` resolves immediately for a past deadline.
                    #timer_outcome_ty::Until(__i) => {
                        ::tokio::time::sleep_until(
                            ::tokio::time::Instant::from_std(__i)
                        ).await;
                    }
                }
                <Self as #timer_trait_path>::after_wait(self, res).await
            }
        },
    };

    let synth = quote! {
        #(#attrs)*
        impl #generics #task_trait_path for #self_ty #where_clause {
            fn config(&self) -> #task_config_ty {
                <Self as #timer_trait_path>::config(self)
            }
            fn name(&self) -> ::std::borrow::Cow<'static, str> {
                <Self as #timer_trait_path>::name(self)
            }
            async fn run(
                &self,
                res: &#resources_ty<#key_ty_tok>,
            ) -> ::std::result::Result<#task_result_ty<#state_ty>, #cano_error_ty> {
                #run_body
            }
        }
    };

    let synth_impl: ItemImpl = parse2(synth)?;
    Ok(async_rewrite::rewrite_impl_block(synth_impl))
}
