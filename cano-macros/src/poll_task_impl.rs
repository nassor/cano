//! Boilerplate-filling pass for `#[cano::poll_task]` on `impl PollTask<S [, K]> for T`
//! blocks and on inherent `impl T { ... }` blocks.
//!
//! ## Why a sibling `impl Task` is emitted
//!
//! A blanket `impl<P: PollTask<..>> Task<..> for P` would conflict (E0119) with the
//! existing `impl<N: Node<..>> Task<..> for N`. Instead, each use of `#[poll_task]` on
//! an **impl** block synthesises a concrete `impl Task<S [, K]> for T` alongside the
//! `PollTask` impl, with `Task::run` delegating to
//! `::cano::task::poll::run_poll_loop(self, res).await`. This gives the same
//! ergonomics ("register a `PollTask` directly with `Workflow::register`") without
//! touching coherence.
//!
//! ## Path handling
//!
//! Same strategy as `router_task_impl.rs`:
//! - Trait-impl form: derive type paths from the same prefix as the user's `PollTask` path.
//! - Inherent form: use `::cano::` paths (valid for external callers).
//!
//! ## Surface forms
//!
//! 1. **Trait-definition form:** `#[poll_task] pub trait PollTask<...> { ... }` —
//!    the macro only async-rewrites the trait.
//!
//! 2. **Trait-impl form:** `#[poll_task] impl PollTask<S> for T { async fn poll(...) }` —
//!    the macro async-rewrites the `PollTask` impl AND emits a companion
//!    `impl Task<S> for T` using the same path prefix as the `PollTask` path.
//!
//! 3. **Inherent-impl form:** `#[poll_task(state = S [, key = K])] impl T { async fn poll(...) }` —
//!    the macro builds the `impl ::cano::PollTask<S [, K]> for T` header, async-rewrites it,
//!    and emits the companion `impl ::cano::Task<S [, K]> for T`.

use proc_macro2::TokenStream;
use quote::quote;
use syn::{
    AngleBracketedGenericArguments, ImplItem, ImplItemFn, ItemImpl, ItemTrait, Path, PathArguments,
    PathSegment, Type, parse2, spanned::Spanned,
};

use crate::async_rewrite;
use crate::attr_args::{AttrArgs, combine_errors};

/// Entry point — dispatches based on what `item` parses to.
pub(crate) fn expand(attr: TokenStream, item: TokenStream) -> syn::Result<TokenStream> {
    if let Ok(trait_def) = parse2::<ItemTrait>(item.clone()) {
        if !attr.is_empty() {
            return Err(syn::Error::new(
                trait_def.span(),
                "#[cano::poll_task]: no attribute args are accepted on a trait definition",
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
                    "#[cano::poll_task] on an inherent `impl T { ... }` block requires \
                     `state = T` (e.g. `#[poll_task(state = MyState)]`)",
                )
            })?;
            return expand_inherent_impl(item_impl, state_ty, args.key);
        } else {
            if args.state.is_some() || args.key.is_some() {
                return Err(syn::Error::new(
                    item_impl.span(),
                    "#[cano::poll_task]: `state` / `key` args only apply to inherent \
                     `impl T { ... }` blocks; when writing `impl PollTask<...> for T` the \
                     trait header already specifies them",
                ));
            }
            return expand_trait_impl(item_impl);
        }
    }

    Err(syn::Error::new(
        proc_macro2::Span::call_site(),
        "#[cano::poll_task]: expected a trait definition or impl block",
    ))
}

// ---------------------------------------------------------------------------
// Trait-impl form: `#[poll_task] impl PollTask<S [, K]> for T { ... }`
// ---------------------------------------------------------------------------

fn expand_trait_impl(item_impl: ItemImpl) -> syn::Result<TokenStream> {
    let (state_ty, key_ty, task_trait_path, module_prefix) =
        extract_state_key_task_path_and_prefix(&item_impl)?;

    let poll_impl = async_rewrite::rewrite_impl_block(item_impl.clone());

    let task_impl = synthesise_task_impl(
        &item_impl,
        &state_ty,
        key_ty.as_ref(),
        task_trait_path,
        &module_prefix,
    )?;

    Ok(quote! {
        #poll_impl
        #task_impl
    })
}

// ---------------------------------------------------------------------------
// Inherent-impl form: `#[poll_task(state = S [, key = K])] impl T { ... }`
// ---------------------------------------------------------------------------

fn expand_inherent_impl(
    item_impl: ItemImpl,
    state_ty: Type,
    key_ty: Option<Type>,
) -> syn::Result<TokenStream> {
    let mut poll_fn: Option<&ImplItemFn> = None;
    let mut errors: Vec<syn::Error> = Vec::new();
    let mut has_config = false;
    let mut has_name = false;
    let mut has_on_poll_error = false;

    for it in &item_impl.items {
        match it {
            ImplItem::Fn(f) => match f.sig.ident.to_string().as_str() {
                "poll" => poll_fn = Some(f),
                "config" => has_config = true,
                "name" => has_name = true,
                "on_poll_error" => has_on_poll_error = true,
                other => {
                    errors.push(syn::Error::new_spanned(
                        &f.sig.ident,
                        format!(
                            "#[cano::poll_task]: unexpected method `{other}` in inherent impl; \
                             only `poll`, `config`, `name`, and `on_poll_error` are allowed"
                        ),
                    ));
                }
            },
            ImplItem::Type(t) => {
                errors.push(syn::Error::new_spanned(
                    &t.ident,
                    format!(
                        "#[cano::poll_task]: unexpected associated type `{}`; \
                         `PollTask` has no associated types",
                        t.ident
                    ),
                ));
            }
            _ => {}
        }
    }

    if poll_fn.is_none() {
        errors.push(syn::Error::new(
            item_impl.span(),
            "#[cano::poll_task] requires an `async fn poll(&self, res: &Resources<_>) \
             -> Result<Poll<_>, CanoError>` method",
        ));
    }

    if !errors.is_empty() {
        return Err(combine_errors(errors));
    }

    let poll_trait_ref: syn::Path = match &key_ty {
        Some(k) => syn::parse_quote!(::cano::PollTask<#state_ty, #k>),
        None => syn::parse_quote!(::cano::PollTask<#state_ty>),
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

    // PollTask defaults to TaskConfig::minimal() (no retries).
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

    let on_poll_error_default = if !has_on_poll_error {
        Some(quote! {
            fn on_poll_error(&self) -> ::cano::PollErrorPolicy {
                ::cano::PollErrorPolicy::FailFast
            }
        })
    } else {
        None
    };

    let synth = quote! {
        #(#attrs)*
        #unsafety impl #generics #poll_trait_ref for #self_ty #where_clause {
            #config_default
            #name_default
            #on_poll_error_default
            #(#user_items)*
        }
    };

    let synth_impl: ItemImpl = parse2(synth)?;
    let poll_impl = async_rewrite::rewrite_impl_block(synth_impl.clone());

    let module_prefix = ModulePrefix::Cano;
    let task_impl = synthesise_task_impl(
        &synth_impl,
        &state_ty,
        key_ty.as_ref(),
        task_trait_path,
        &module_prefix,
    )?;

    Ok(quote! {
        #poll_impl
        #task_impl
    })
}

// ---------------------------------------------------------------------------
// Module prefix — determines how cano types are referenced in the companion impl
// ---------------------------------------------------------------------------

enum ModulePrefix {
    Cano,
    Prefixed(proc_macro2::TokenStream),
    Bare,
}

impl ModulePrefix {
    fn qualify(&self, type_name: &str) -> proc_macro2::TokenStream {
        let ident = syn::Ident::new(type_name, proc_macro2::Span::call_site());
        match self {
            ModulePrefix::Cano => quote! { ::cano::#ident },
            ModulePrefix::Prefixed(prefix) => quote! { #prefix #ident },
            ModulePrefix::Bare => quote! { #ident },
        }
    }
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
                "PollTask impl must have angle-bracketed type arguments (e.g. `PollTask<MyState>`)",
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
            "PollTask impl requires at least one type argument (the state type)",
        ));
    }

    let state_ty = type_args[0].clone();
    let key_ty = type_args.get(1).map(|t| (*t).clone());

    let module_prefix = derive_module_prefix(trait_path);
    let task_path = derive_task_path_from_poll_path(trait_path, &state_ty, key_ty.as_ref())?;

    Ok((state_ty, key_ty, task_path, module_prefix))
}

fn derive_module_prefix(poll_path: &Path) -> ModulePrefix {
    let seg_count = poll_path.segments.len();

    if seg_count <= 1 && poll_path.leading_colon.is_none() {
        return ModulePrefix::Bare;
    }

    let prefix_segs: syn::punctuated::Punctuated<PathSegment, syn::token::PathSep> = poll_path
        .segments
        .iter()
        .take(seg_count.saturating_sub(1))
        .cloned()
        .collect();

    let has_leading_colon = poll_path.leading_colon.is_some();
    let is_cano_prefix = prefix_segs.len() == 1
        && prefix_segs
            .iter()
            .next()
            .map(|s| s.ident == "cano")
            .unwrap_or(false);

    if has_leading_colon && is_cano_prefix {
        return ModulePrefix::Cano;
    }

    let leading = if has_leading_colon {
        quote! { :: }
    } else {
        quote! {}
    };

    ModulePrefix::Prefixed(quote! { #leading #prefix_segs :: })
}

fn derive_task_path_from_poll_path(
    poll_path: &Path,
    state_ty: &Type,
    key_ty: Option<&Type>,
) -> syn::Result<Path> {
    let mut task_path = poll_path.clone();

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

/// Synthesise `impl Task<S [, K]> for T` whose `Task::run` drives the poll loop,
/// forwarding `config`/`name` to `PollTask::config`/`PollTask::name` via UFCS.
///
/// For the inherent form (and other cases where `::cano::` is valid), `Task::run`
/// delegates to `::cano::task::poll::run_poll_loop(self, res).await`. For the
/// trait-impl form used inside the `cano` crate itself (where `::cano` doesn't
/// exist), the poll loop is inlined directly so no external crate reference is needed.
fn synthesise_task_impl(
    poll_impl: &ItemImpl,
    state_ty: &Type,
    key_ty: Option<&Type>,
    task_trait_path: Path,
    module_prefix: &ModulePrefix,
) -> syn::Result<TokenStream> {
    let attrs = &poll_impl.attrs;
    let generics = &poll_impl.generics;
    let where_clause = &poll_impl.generics.where_clause;
    let self_ty = &poll_impl.self_ty;

    let (_, poll_trait_path, _) = poll_impl
        .trait_
        .as_ref()
        .ok_or_else(|| syn::Error::new(poll_impl.span(), "expected a PollTask trait impl block"))?;

    let task_config_ty = module_prefix.qualify("TaskConfig");
    let resources_ty = module_prefix.qualify("Resources");
    let task_result_ty = module_prefix.qualify("TaskResult");
    let cano_error_ty = module_prefix.qualify("CanoError");
    let poll_ty = module_prefix.qualify("Poll");
    let poll_error_policy_ty = module_prefix.qualify("PollErrorPolicy");

    let key_ty_tok: TokenStream = match key_ty {
        Some(k) => quote! { #k },
        None => quote! { ::std::borrow::Cow<'static, str> },
    };

    // For the inherent form (ModulePrefix::Cano), delegate to the free helper function
    // in cano::task::poll so that the loop body lives in one place.
    //
    // For the trait-impl form (Bare / Prefixed), inline the loop so it compiles both
    // inside the `cano` crate (where `::cano` does not resolve) and in external crates.
    let run_body = match module_prefix {
        ModulePrefix::Cano => quote! {
            ::cano::task::poll::run_poll_loop(self, res).await
        },
        _ => quote! {
            {
                let __policy = <Self as #poll_trait_path>::on_poll_error(self);
                let mut __consecutive_errors: u32 = 0;
                loop {
                    match <Self as #poll_trait_path>::poll(self, res).await {
                        Ok(#poll_ty::Ready(result)) => return Ok(result),
                        Ok(#poll_ty::Pending { delay_ms }) => {
                            __consecutive_errors = 0;
                            if delay_ms > 0 {
                                ::tokio::time::sleep(
                                    ::std::time::Duration::from_millis(delay_ms)
                                ).await;
                            }
                        }
                        Err(__e) => match &__policy {
                            #poll_error_policy_ty::FailFast => return Err(__e),
                            #poll_error_policy_ty::RetryOnError { max_errors: __max } => {
                                __consecutive_errors += 1;
                                if __consecutive_errors > *__max {
                                    return Err(__e);
                                }
                            }
                        },
                    }
                }
            }
        },
    };

    let synth = quote! {
        #(#attrs)*
        impl #generics #task_trait_path for #self_ty #where_clause {
            fn config(&self) -> #task_config_ty {
                <Self as #poll_trait_path>::config(self)
            }
            fn name(&self) -> ::std::borrow::Cow<'static, str> {
                <Self as #poll_trait_path>::name(self)
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
