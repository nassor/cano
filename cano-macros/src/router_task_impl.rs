//! Boilerplate-filling pass for `#[cano::router_task]` on `impl RouterTask<S [, K]> for T`
//! blocks and on inherent `impl T { ... }` blocks.
//!
//! ## Why a sibling `impl Task` is emitted
//!
//! A blanket `impl<R: RouterTask<..>> Task<..> for R` would conflict (E0119) with the
//! existing `impl<N: Node<..>> Task<..> for N`. Instead, each use of `#[router_task]` on
//! an **impl** block synthesises a concrete `impl Task<S [, K]> for T` alongside the
//! `RouterTask` impl, delegating `Task::run` → `RouterTask::route`. This gives the same
//! ergonomics ("register a `RouterTask` directly with `Workflow::register`") without
//! touching coherence.
//!
//! ## Path handling
//!
//! The companion `impl Task<S> for T` must use type references that resolve in the same
//! scope as the caller's `RouterTask` path. Strategy:
//! - Trait-impl form: the user wrote `impl [prefix::] RouterTask<S> for T`. The companion
//!   derives type paths from the same prefix (e.g. `prefix::Task`, `prefix::TaskConfig`,
//!   `prefix::Resources`, etc.). An empty prefix means bare names (which must be in scope).
//! - Inherent form: the macro builds `impl ::cano::RouterTask<S> for T` and uses
//!   `::cano::Task`, `::cano::TaskConfig`, etc. (valid for external callers).
//!
//! ## Surface forms
//!
//! 1. **Trait-definition form:** `#[router_task] pub trait RouterTask<...> { ... }` —
//!    the macro only async-rewrites the trait.
//!
//! 2. **Trait-impl form:** `#[router_task] impl RouterTask<S> for T { async fn route(...) }` —
//!    the macro async-rewrites the `RouterTask` impl AND emits a companion
//!    `impl Task<S> for T` using the same path prefix as the `RouterTask` path.
//!
//! 3. **Inherent-impl form:** `#[router_task(state = S [, key = K])] impl T { async fn route(...) }` —
//!    the macro builds the `impl ::cano::RouterTask<S [, K]> for T` header, async-rewrites it,
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
    // Check whether this is a trait definition, an impl block, or something else.
    if let Ok(trait_def) = parse2::<ItemTrait>(item.clone()) {
        // Trait-definition form: only async-rewrite.
        if !attr.is_empty() {
            return Err(syn::Error::new(
                trait_def.span(),
                "#[cano::router_task]: no attribute args are accepted on a trait definition",
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
                    "#[cano::router_task] on an inherent `impl T { ... }` block requires \
                     `state = T` (e.g. `#[router_task(state = MyState)]`)",
                )
            })?;
            return expand_inherent_impl(item_impl, state_ty, args.key);
        } else {
            // Trait-impl form: attr args not accepted (trait header already specifies them).
            if args.state.is_some() || args.key.is_some() {
                return Err(syn::Error::new(
                    item_impl.span(),
                    "#[cano::router_task]: `state` / `key` args only apply to inherent \
                     `impl T { ... }` blocks; when writing `impl RouterTask<...> for T` the \
                     trait header already specifies them",
                ));
            }
            return expand_trait_impl(item_impl);
        }
    }

    Err(syn::Error::new(
        proc_macro2::Span::call_site(),
        "#[cano::router_task]: expected a trait definition or impl block",
    ))
}

// ---------------------------------------------------------------------------
// Trait-impl form: `#[router_task] impl RouterTask<S [, K]> for T { ... }`
// ---------------------------------------------------------------------------

fn expand_trait_impl(item_impl: ItemImpl) -> syn::Result<TokenStream> {
    // Extract the state and optional key types from the trait path in the impl header,
    // so we can synthesise the companion Task impl.
    let (state_ty, key_ty, task_trait_path, module_prefix) =
        extract_state_key_task_path_and_prefix(&item_impl)?;

    // Async-rewrite the RouterTask impl.
    // No need to inject defaults — the RouterTask trait already provides them.
    let router_impl = async_rewrite::rewrite_impl_block(item_impl.clone());

    // Synthesise companion `impl Task<S [, K]> for T`, using the task_trait_path that
    // was derived from the caller's RouterTask path (same prefix, RouterTask → Task).
    let task_impl = synthesise_task_impl(
        &item_impl,
        &state_ty,
        key_ty.as_ref(),
        task_trait_path,
        &module_prefix,
    )?;

    Ok(quote! {
        #router_impl
        #task_impl
    })
}

// ---------------------------------------------------------------------------
// Inherent-impl form: `#[router_task(state = S [, key = K])] impl T { ... }`
// ---------------------------------------------------------------------------

fn expand_inherent_impl(
    item_impl: ItemImpl,
    state_ty: Type,
    key_ty: Option<Type>,
) -> syn::Result<TokenStream> {
    // Validate items: only `route`, `config`, `name` are allowed.
    let mut route_fn: Option<&ImplItemFn> = None;
    let mut errors: Vec<syn::Error> = Vec::new();
    let mut has_config = false;
    let mut has_name = false;

    for it in &item_impl.items {
        match it {
            ImplItem::Fn(f) => match f.sig.ident.to_string().as_str() {
                "route" => route_fn = Some(f),
                "config" => has_config = true,
                "name" => has_name = true,
                other => {
                    errors.push(syn::Error::new_spanned(
                        &f.sig.ident,
                        format!(
                            "#[cano::router_task]: unexpected method `{other}` in inherent impl; \
                             only `route`, `config`, and `name` are allowed"
                        ),
                    ));
                }
            },
            ImplItem::Type(t) => {
                errors.push(syn::Error::new_spanned(
                    &t.ident,
                    format!(
                        "#[cano::router_task]: unexpected associated type `{}`; \
                         `RouterTask` has no associated types",
                        t.ident
                    ),
                ));
            }
            _ => {}
        }
    }

    if route_fn.is_none() {
        errors.push(syn::Error::new(
            item_impl.span(),
            "#[cano::router_task] requires an `async fn route(&self, res: &Resources<_>) \
             -> Result<TaskResult<_>, CanoError>` method",
        ));
    }

    if !errors.is_empty() {
        return Err(combine_errors(errors));
    }

    // Build the RouterTask trait reference with `::cano::` prefix (works for external users;
    // internal cano library tests must use the trait-impl form).
    let router_trait_ref: syn::Path = match &key_ty {
        Some(k) => syn::parse_quote!(::cano::RouterTask<#state_ty, #k>),
        None => syn::parse_quote!(::cano::RouterTask<#state_ty>),
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

    // Inject default config/name if the user didn't write them.
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

    // Synthesise the RouterTask trait impl.
    let synth = quote! {
        #(#attrs)*
        #unsafety impl #generics #router_trait_ref for #self_ty #where_clause {
            #config_default
            #name_default
            #(#user_items)*
        }
    };

    let synth_impl: ItemImpl = parse2(synth)?;
    let router_impl = async_rewrite::rewrite_impl_block(synth_impl.clone());

    // Synthesise the companion Task impl using `::cano::` prefix (same as RouterTask above).
    // The module prefix is `::cano::` for the inherent form.
    let module_prefix = ModulePrefix::Cano;
    let task_impl = synthesise_task_impl(
        &synth_impl,
        &state_ty,
        key_ty.as_ref(),
        task_trait_path,
        &module_prefix,
    )?;

    Ok(quote! {
        #router_impl
        #task_impl
    })
}

// ---------------------------------------------------------------------------
// Module prefix — determines how cano types are referenced in the companion impl
// ---------------------------------------------------------------------------

/// The path prefix to use when referencing cano types in the companion `impl Task`.
///
/// - `Cano` → `::cano::TypeName` (for the inherent form or external users)
/// - `Prefixed(path)` → `prefix::TypeName` (e.g. `cano::TypeName`)
/// - `Bare` → unqualified `TypeName` (for the trait-impl form inside the cano crate itself,
///   where types are in scope via `use super::*` or similar imports)
enum ModulePrefix {
    /// Use `::cano::TypeName`.
    Cano,
    /// Use `prefix::TypeName` where prefix is the path before the last segment.
    Prefixed(proc_macro2::TokenStream),
    /// Use bare `TypeName` (name only, no prefix).
    Bare,
}

impl ModulePrefix {
    /// Build a qualified path token stream: `{prefix}TypeName`.
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

/// Extract `TState`, optional `TResourceKey`, the companion `Task<S[,K]>` path,
/// and the module prefix from the trait path in an impl header.
///
/// The module prefix is derived from all path segments except the last one.
/// E.g.:
/// - `RouterTask<S>` → prefix = `Bare`, task path = `Task<S>`
/// - `cano::RouterTask<S>` → prefix = `Prefixed(cano::)`, task path = `cano::Task<S>`
/// - `::cano::RouterTask<S>` → prefix = `Cano`, task path = `::cano::Task<S>`
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
                "RouterTask impl must have angle-bracketed type arguments (e.g. `RouterTask<MyState>`)",
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
            "RouterTask impl requires at least one type argument (the state type)",
        ));
    }

    let state_ty = type_args[0].clone();
    let key_ty = type_args.get(1).map(|t| (*t).clone());

    // Determine the module prefix.
    let module_prefix = derive_module_prefix(trait_path);

    // Build the companion Task path: same prefix as RouterTask, last segment → Task.
    let task_path = derive_task_path_from_router_path(trait_path, &state_ty, key_ty.as_ref())?;

    Ok((state_ty, key_ty, task_path, module_prefix))
}

/// Derive a `ModulePrefix` from a `RouterTask<S>` path.
fn derive_module_prefix(router_path: &Path) -> ModulePrefix {
    let seg_count = router_path.segments.len();

    if seg_count <= 1 && router_path.leading_colon.is_none() {
        // No prefix (bare `RouterTask<S>`).
        return ModulePrefix::Bare;
    }

    // Collect all segments except the last one as the prefix.
    let prefix_segs: syn::punctuated::Punctuated<PathSegment, syn::token::PathSep> = router_path
        .segments
        .iter()
        .take(seg_count.saturating_sub(1))
        .cloned()
        .collect();

    // Check if this is `::cano::` (leading colon + single "cano" prefix segment).
    let has_leading_colon = router_path.leading_colon.is_some();
    let is_cano_prefix = prefix_segs.len() == 1
        && prefix_segs
            .iter()
            .next()
            .map(|s| s.ident == "cano")
            .unwrap_or(false);

    if has_leading_colon && is_cano_prefix {
        return ModulePrefix::Cano;
    }

    // Build a token stream for the prefix (e.g. `cano::` or `foo::bar::`).
    let leading = if has_leading_colon {
        quote! { :: }
    } else {
        quote! {}
    };

    ModulePrefix::Prefixed(quote! { #leading #prefix_segs :: })
}

/// Build a `Task<S[,K]>` path with the same path prefix as the given `RouterTask<S[,K]>` path.
fn derive_task_path_from_router_path(
    router_path: &Path,
    state_ty: &Type,
    key_ty: Option<&Type>,
) -> syn::Result<Path> {
    let mut task_path = router_path.clone();

    // Replace the last segment with Task<S[,K]>.
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

/// Synthesise `impl Task<S [, K]> for T` that delegates `Task::run` to `RouterTask::route`,
/// and forwards `config`/`name` to `RouterTask::config`/`RouterTask::name` via UFCS.
///
/// The companion impl is written as `async fn run` and piped through `async_rewrite`,
/// avoiding the need to manually spell out the boxed-future return type.
///
/// All cano type references (`TaskConfig`, `Resources`, `TaskResult`, `CanoError`) are
/// qualified using `module_prefix` so they resolve correctly both inside and outside the
/// cano crate.
fn synthesise_task_impl(
    router_impl: &ItemImpl,
    state_ty: &Type,
    key_ty: Option<&Type>,
    task_trait_path: Path,
    module_prefix: &ModulePrefix,
) -> syn::Result<TokenStream> {
    let attrs = &router_impl.attrs;
    let generics = &router_impl.generics;
    let where_clause = &router_impl.generics.where_clause;
    let self_ty = &router_impl.self_ty;

    // The RouterTask path is the trait path from the impl header — used for UFCS calls.
    let (_, router_trait_path, _) = router_impl.trait_.as_ref().ok_or_else(|| {
        syn::Error::new(router_impl.span(), "expected a RouterTask trait impl block")
    })?;

    // Derive qualified type references.
    let task_config_ty = module_prefix.qualify("TaskConfig");
    let resources_ty = module_prefix.qualify("Resources");
    let task_result_ty = module_prefix.qualify("TaskResult");
    let cano_error_ty = module_prefix.qualify("CanoError");

    // Resource key type — needed for the `res` parameter.
    let key_ty_tok: TokenStream = match key_ty {
        Some(k) => quote! { #k },
        None => quote! { ::std::borrow::Cow<'static, str> },
    };

    // Emit the companion as an `async fn run` impl block, then pipe through async_rewrite.
    // This avoids manually spelling out the `Pin<Box<dyn Future>>` return type.
    let synth = quote! {
        #(#attrs)*
        impl #generics #task_trait_path for #self_ty #where_clause {
            fn config(&self) -> #task_config_ty {
                <Self as #router_trait_path>::config(self)
            }
            fn name(&self) -> ::std::borrow::Cow<'static, str> {
                <Self as #router_trait_path>::name(self)
            }
            async fn run(
                &self,
                res: &#resources_ty<#key_ty_tok>,
            ) -> ::std::result::Result<#task_result_ty<#state_ty>, #cano_error_ty> {
                <Self as #router_trait_path>::route(self, res).await
            }
        }
    };

    let synth_impl: ItemImpl = parse2(synth)?;
    Ok(async_rewrite::rewrite_impl_block(synth_impl))
}
