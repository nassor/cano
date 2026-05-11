//! Boilerplate-filling pass for `#[cano::stepped_task]` on `impl SteppedTask<S [, K]> for T`
//! blocks and on inherent `impl T { ... }` blocks.
//!
//! ## Why a sibling `impl Task` is emitted
//!
//! A blanket `impl<S: SteppedTask<..>> Task<..> for S` would conflict (E0119) with the
//! existing `impl<N: Node<..>> Task<..> for N`. Instead, each use of `#[stepped_task]` on
//! an **impl** block synthesises a concrete `impl Task<S [, K]> for T` alongside the
//! `SteppedTask` impl, with `Task::run` delegating to
//! `::cano::task::stepped::run_stepped(self, res).await`. This gives the same
//! ergonomics ("register a `SteppedTask` directly with `Workflow::register`") without
//! touching coherence.
//!
//! ## Path handling
//!
//! Same strategy as `poll_task_impl.rs`:
//! - Trait-impl form: derive type paths from the same prefix as the user's `SteppedTask` path.
//! - Inherent form: use `::cano::` paths (valid for external callers).
//!
//! ## Cursor inference (inherent form)
//!
//! The inherent form infers `type Cursor` from the second parameter of `step`:
//! `Option<T>` → `T`. The `peel_option_param` helper extracts `T` from `Option<T>`.
//!
//! ## Surface forms
//!
//! 1. **Trait-definition form:** `#[stepped_task] pub trait SteppedTask<...> { ... }` —
//!    the macro only async-rewrites the trait.
//!
//! 2. **Trait-impl form:** `#[stepped_task] impl SteppedTask<S> for T { type Cursor = C; async fn step(...) }` —
//!    the macro async-rewrites the `SteppedTask` impl AND emits a companion
//!    `impl Task<S> for T` using the same path prefix as the `SteppedTask` path.
//!
//! 3. **Inherent-impl form:** `#[stepped_task(state = S [, key = K])] impl T { async fn step(...) }` —
//!    the macro builds the `impl ::cano::SteppedTask<S [, K]> for T` header, infers
//!    `type Cursor` from the `Option<C>` second parameter of `step`, async-rewrites it,
//!    and emits the companion `impl ::cano::Task<S [, K]> for T`.

use proc_macro2::TokenStream;
use quote::quote;
use syn::{
    AngleBracketedGenericArguments, FnArg, GenericArgument, ImplItem, ImplItemFn, ItemImpl,
    ItemTrait, Path, PathArguments, PathSegment, Type, parse2, spanned::Spanned,
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
                "#[cano::stepped_task]: no attribute args are accepted on a trait definition",
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
                    "#[cano::stepped_task] on an inherent `impl T { ... }` block requires \
                     `state = T` (e.g. `#[stepped_task(state = MyState)]`)",
                )
            })?;
            return expand_inherent_impl(item_impl, state_ty, args.key);
        } else {
            if args.state.is_some() || args.key.is_some() {
                return Err(syn::Error::new(
                    item_impl.span(),
                    "#[cano::stepped_task]: `state` / `key` args only apply to inherent \
                     `impl T { ... }` blocks; when writing `impl SteppedTask<...> for T` the \
                     trait header already specifies them",
                ));
            }
            return expand_trait_impl(item_impl);
        }
    }

    Err(syn::Error::new(
        proc_macro2::Span::call_site(),
        "#[cano::stepped_task]: expected a trait definition or impl block",
    ))
}

// ---------------------------------------------------------------------------
// Trait-impl form: `#[stepped_task] impl SteppedTask<S [, K]> for T { ... }`
// ---------------------------------------------------------------------------

fn expand_trait_impl(item_impl: ItemImpl) -> syn::Result<TokenStream> {
    let (state_ty, key_ty, task_trait_path, module_prefix) =
        extract_state_key_task_path_and_prefix(&item_impl)?;

    let stepped_impl = async_rewrite::rewrite_impl_block(item_impl.clone());

    let task_impl = synthesise_task_impl(
        &item_impl,
        &state_ty,
        key_ty.as_ref(),
        task_trait_path,
        &module_prefix,
    )?;

    Ok(quote! {
        #stepped_impl
        #task_impl
    })
}

// ---------------------------------------------------------------------------
// Inherent-impl form: `#[stepped_task(state = S [, key = K])] impl T { ... }`
// ---------------------------------------------------------------------------

fn expand_inherent_impl(
    item_impl: ItemImpl,
    state_ty: Type,
    key_ty: Option<Type>,
) -> syn::Result<TokenStream> {
    let mut step_fn: Option<&ImplItemFn> = None;
    let mut errors: Vec<syn::Error> = Vec::new();
    let mut has_cursor_ty = false;
    let mut has_config = false;
    let mut has_name = false;

    for it in &item_impl.items {
        match it {
            ImplItem::Fn(f) => match f.sig.ident.to_string().as_str() {
                "step" => step_fn = Some(f),
                "config" => has_config = true,
                "name" => has_name = true,
                other => {
                    errors.push(syn::Error::new_spanned(
                        &f.sig.ident,
                        format!(
                            "#[cano::stepped_task]: unexpected method `{other}` in inherent impl; \
                             only `step`, `config`, and `name` are allowed"
                        ),
                    ));
                }
            },
            ImplItem::Type(t) => match t.ident.to_string().as_str() {
                "Cursor" => has_cursor_ty = true,
                other => {
                    errors.push(syn::Error::new_spanned(
                        &t.ident,
                        format!(
                            "#[cano::stepped_task]: unexpected associated type `{other}`; \
                             only `Cursor` is recognised (and it can be inferred)"
                        ),
                    ));
                }
            },
            _ => {}
        }
    }

    if step_fn.is_none() {
        errors.push(syn::Error::new(
            item_impl.span(),
            "#[cano::stepped_task] requires an \
             `async fn step(&self, res: &Resources<_>, cursor: Option<_>) \
             -> Result<StepOutcome<_, _>, CanoError>` method",
        ));
    }

    if !errors.is_empty() {
        return Err(combine_errors(errors));
    }

    let step = step_fn.unwrap();

    // Infer `Cursor` from the `Option<T>` second parameter of `step` (i.e. `Option<T>` → `T`).
    let cursor_inj = if has_cursor_ty {
        None
    } else {
        let inferred = peel_option_param(step, "step")?;
        Some(quote!(type Cursor = #inferred;))
    };

    let stepped_trait_ref: syn::Path = match &key_ty {
        Some(k) => syn::parse_quote!(::cano::SteppedTask<#state_ty, #k>),
        None => syn::parse_quote!(::cano::SteppedTask<#state_ty>),
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

    // SteppedTask defaults to TaskConfig::default() (exponential backoff).
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
        #unsafety impl #generics #stepped_trait_ref for #self_ty #where_clause {
            #cursor_inj
            #config_default
            #name_default
            #(#user_items)*
        }
    };

    let synth_impl: ItemImpl = parse2(synth)?;
    let stepped_impl = async_rewrite::rewrite_impl_block(synth_impl.clone());

    let module_prefix = ModulePrefix::Cano;
    let task_impl = synthesise_task_impl(
        &synth_impl,
        &state_ty,
        key_ty.as_ref(),
        task_trait_path,
        &module_prefix,
    )?;

    Ok(quote! {
        #stepped_impl
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
                "SteppedTask impl must have angle-bracketed type arguments \
                 (e.g. `SteppedTask<MyState>`)",
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
            "SteppedTask impl requires at least one type argument (the state type)",
        ));
    }

    let state_ty = type_args[0].clone();
    let key_ty = type_args.get(1).map(|t| (*t).clone());

    let module_prefix = derive_module_prefix(trait_path);
    let task_path = derive_task_path_from_stepped_path(trait_path, &state_ty, key_ty.as_ref())?;

    Ok((state_ty, key_ty, task_path, module_prefix))
}

fn derive_task_path_from_stepped_path(
    stepped_path: &Path,
    state_ty: &Type,
    key_ty: Option<&Type>,
) -> syn::Result<Path> {
    let mut task_path = stepped_path.clone();

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

/// Synthesise `impl Task<S [, K]> for T` whose `Task::run` drives the step loop,
/// forwarding `config`/`name` to `SteppedTask::config`/`SteppedTask::name` via UFCS.
///
/// ## Why `Task::run` uses a desugared signature (not `async fn`) for the Cano form
///
/// The `async_rewrite` pass wraps every `async fn run` body in
/// `Box::pin(async move { ... })` with a `for<'async_trait>` universal lifetime binder.
/// When the body calls `run_stepped(self, res).await`, the compiler must satisfy
/// `for<'0> SteppedTask<S, Cow<'0, str>>`, but the impl only provides
/// `SteppedTask<S, Cow<'static, str>>` (the default). This produces:
///
/// > "implementation of `SteppedTask` is not general enough"
///
/// The fix (same as `batch_task_impl.rs`): write the desugared `fn run` signature by
/// hand and use `Box::pin(run_stepped(self, res))` — no extra `async move` wrapper.
///
/// For the trait-impl / Bare / Prefixed form (inside `cano` crate itself, or nested
/// modules), we inline the loop as an `async fn run` body since `::cano` doesn't
/// resolve there, and the HRTB issue doesn't surface when types are bare.
fn synthesise_task_impl(
    stepped_impl: &ItemImpl,
    state_ty: &Type,
    key_ty: Option<&Type>,
    task_trait_path: Path,
    module_prefix: &ModulePrefix,
) -> syn::Result<TokenStream> {
    let attrs = &stepped_impl.attrs;
    let generics = &stepped_impl.generics;
    let where_clause = &stepped_impl.generics.where_clause;
    let self_ty = &stepped_impl.self_ty;

    let (_, stepped_trait_path, _) = stepped_impl.trait_.as_ref().ok_or_else(|| {
        syn::Error::new(
            stepped_impl.span(),
            "expected a SteppedTask trait impl block",
        )
    })?;

    let task_config_ty = module_prefix.qualify("TaskConfig");
    let resources_ty = module_prefix.qualify("Resources");
    let task_result_ty = module_prefix.qualify("TaskResult");
    let cano_error_ty = module_prefix.qualify("CanoError");
    let step_ty = module_prefix.qualify("StepOutcome");

    let key_ty_tok: TokenStream = match key_ty {
        Some(k) => quote! { #k },
        None => quote! { ::std::borrow::Cow<'static, str> },
    };

    // For the inherent form (ModulePrefix::Cano), use the desugared `fn run` signature
    // to avoid the HRTB "not general enough" error.
    //
    // For the trait-impl form (Bare / Prefixed), inline the loop as an `async fn run`
    // so it compiles inside the `cano` crate where `::cano` doesn't resolve.
    let synth = match module_prefix {
        ModulePrefix::Cano => quote! {
            #(#attrs)*
            impl #generics #task_trait_path for #self_ty #where_clause {
                fn config(&self) -> #task_config_ty {
                    <Self as #stepped_trait_path>::config(self)
                }
                fn name(&self) -> ::std::borrow::Cow<'static, str> {
                    <Self as #stepped_trait_path>::name(self)
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
                    ::std::boxed::Box::pin(
                        ::cano::task::stepped::run_stepped(self, res)
                    )
                }
            }
        },
        _ => {
            let synth_inner = quote! {
                #(#attrs)*
                impl #generics #task_trait_path for #self_ty #where_clause {
                    fn config(&self) -> #task_config_ty {
                        <Self as #stepped_trait_path>::config(self)
                    }
                    fn name(&self) -> ::std::borrow::Cow<'static, str> {
                        <Self as #stepped_trait_path>::name(self)
                    }
                    async fn run(
                        &self,
                        res: &#resources_ty<#key_ty_tok>,
                    ) -> ::std::result::Result<#task_result_ty<#state_ty>, #cano_error_ty> {
                        let mut __cursor = None;
                        loop {
                            match <Self as #stepped_trait_path>::step(self, res, __cursor).await? {
                                #step_ty::More(__new_cursor) => {
                                    __cursor = Some(__new_cursor);
                                }
                                #step_ty::Done(__result) => return Ok(__result),
                            }
                        }
                    }
                }
            };
            let synth_impl: ItemImpl = parse2(synth_inner)?;
            return Ok(async_rewrite::rewrite_impl_block(synth_impl));
        }
    };

    // For the Cano form the signature is already desugared — do NOT run async_rewrite.
    parse2(synth).map(|impl_block: ItemImpl| quote! { #impl_block })
}

// ---------------------------------------------------------------------------
// Type-inference helpers
// ---------------------------------------------------------------------------

/// Extract `T` from the second parameter `cursor: Option<T>` of `step`.
///
/// The first parameter is `&self`; the second should be `res: &Resources<_>`;
/// the third should be `cursor: Option<C>`. We look at the third parameter.
fn peel_option_param(f: &ImplItemFn, fn_name: &str) -> syn::Result<Type> {
    // Skip `&self` (first) and `res: &Resources<_>` (second); get `cursor: Option<T>` (third).
    let third = f.sig.inputs.iter().nth(2).ok_or_else(|| {
        syn::Error::new_spanned(
            &f.sig,
            format!(
                "#[cano::stepped_task]: `{fn_name}` must have a third parameter \
                 `cursor: Option<T>` from which `type Cursor` can be inferred"
            ),
        )
    })?;

    let ty = match third {
        FnArg::Typed(pt) => &*pt.ty,
        FnArg::Receiver(_) => {
            return Err(syn::Error::new_spanned(
                third,
                format!(
                    "#[cano::stepped_task]: `{fn_name}` third parameter must be a typed argument, not `self`"
                ),
            ));
        }
    };

    // Expect `Option<T>`.
    let Type::Path(tp) = ty else {
        return Err(syn::Error::new_spanned(
            ty,
            format!(
                "#[cano::stepped_task]: `{fn_name}` third parameter must be `Option<T>` \
                 so that `type Cursor = T` can be inferred; found `{}`",
                quote! { #ty }
            ),
        ));
    };

    let last = tp.path.segments.last().ok_or_else(|| {
        syn::Error::new_spanned(
            ty,
            format!("#[cano::stepped_task]: cannot read type of `{fn_name}` third parameter"),
        )
    })?;

    if last.ident != "Option" {
        return Err(syn::Error::new_spanned(
            ty,
            format!(
                "#[cano::stepped_task]: `{fn_name}` third parameter must be `Option<T>` \
                 so that `type Cursor = T` can be inferred; found `{}`",
                quote! { #ty }
            ),
        ));
    }

    let PathArguments::AngleBracketed(args) = &last.arguments else {
        return Err(syn::Error::new_spanned(
            ty,
            format!(
                "#[cano::stepped_task]: `{fn_name}` third parameter `Option` must have \
                 an angle-bracketed type argument"
            ),
        ));
    };

    let inner_types: Vec<&Type> = args
        .args
        .iter()
        .filter_map(|a| {
            if let GenericArgument::Type(t) = a {
                Some(t)
            } else {
                None
            }
        })
        .collect();

    if inner_types.is_empty() {
        return Err(syn::Error::new_spanned(
            ty,
            format!(
                "#[cano::stepped_task]: `{fn_name}` third parameter `Option` must have \
                 exactly one type argument"
            ),
        ));
    }

    Ok(inner_types[0].clone())
}
