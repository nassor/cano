//! Boilerplate-filling pass for `#[cano::task::stream]` on `impl StreamTask<S [, K]> for T`
//! blocks and on inherent `impl T { ... }` blocks.
//!
//! ## Why a sibling `impl Task` is emitted
//!
//! Like the other specialized task traits, each use of `#[stream_task]` on an **impl**
//! block synthesises a concrete `impl Task<S [, K]> for T` alongside the `StreamTask`
//! impl. `Task::run` forwards to the provided method `StreamTask::run_in_memory`, which
//! drives the in-memory windowed loop (no cursor persistence, no cancellation). This gives
//! the "register a `StreamTask` directly with `Workflow::register`" ergonomics without
//! touching coherence. The durable / cancellable path is `Workflow::register_stream`.
//!
//! Forwarding to a single provided method (rather than inlining the loop per module
//! prefix, as `#[task::poll]`/`#[task::stepped]` do) keeps exactly one loop body and
//! sidesteps the HRTB "implementation is not general enough" error, because the companion
//! `Task::run` is a hand-desugared `fn` (no `async move` binder) that simply returns the
//! pinned future produced by `run_in_memory`.
//!
//! ## Type inference (inherent form)
//!
//! - `type Item`   ← the owned third parameter of `process_item` (`item: T` → `T`).
//! - `type Output` ← the first element of the `Ok` tuple of `process_item`'s
//!   `Result<(Output, Cursor), _>` return.
//! - `type Cursor` ← the second element of that tuple.
//!
//! ## Surface forms
//!
//! 1. **Trait-definition form:** `#[stream_task] pub trait StreamTask<...> { ... }` —
//!    the macro only async-rewrites the trait.
//! 2. **Trait-impl form:** `#[stream_task] impl StreamTask<S> for T { type Item = ..; ... }`.
//! 3. **Inherent-impl form:** `#[stream_task(state = S [, key = K])] impl T { async fn open(..) .. }`.

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
                "#[cano::task::stream]: no attribute args are accepted on a trait definition",
            ));
        }
        let rewritten = async_rewrite::rewrite_trait_def(trait_def);
        return Ok(quote! { #rewritten });
    }

    if let Ok(item_impl) = parse2::<ItemImpl>(item.clone()) {
        let args = AttrArgs::parse(attr)?;

        if item_impl.trait_.is_none() {
            let state_ty = args.state.ok_or_else(|| {
                syn::Error::new(
                    item_impl.span(),
                    "#[cano::task::stream] on an inherent `impl T { ... }` block requires \
                     `state = T` (e.g. `#[task::stream(state = MyState)]`)",
                )
            })?;
            return expand_inherent_impl(item_impl, state_ty, args.key);
        } else {
            if args.state.is_some() || args.key.is_some() {
                return Err(syn::Error::new(
                    item_impl.span(),
                    "#[cano::task::stream]: `state` / `key` args only apply to inherent \
                     `impl T { ... }` blocks; when writing `impl StreamTask<...> for T` the \
                     trait header already specifies them",
                ));
            }
            return expand_trait_impl(item_impl);
        }
    }

    Err(syn::Error::new(
        proc_macro2::Span::call_site(),
        "#[cano::task::stream]: expected a trait definition or impl block",
    ))
}

// ---------------------------------------------------------------------------
// Trait-impl form: `#[stream_task] impl StreamTask<S [, K]> for T { ... }`
// ---------------------------------------------------------------------------

fn expand_trait_impl(item_impl: ItemImpl) -> syn::Result<TokenStream> {
    let (state_ty, key_ty, task_trait_path, module_prefix) =
        extract_state_key_task_path_and_prefix(&item_impl)?;

    let stream_impl = async_rewrite::rewrite_impl_block(item_impl.clone());

    let task_impl = synthesise_task_impl(
        &item_impl,
        &state_ty,
        key_ty.as_ref(),
        task_trait_path,
        &module_prefix,
    )?;

    Ok(quote! {
        #stream_impl
        #task_impl
    })
}

// ---------------------------------------------------------------------------
// Inherent-impl form: `#[stream_task(state = S [, key = K])] impl T { ... }`
// ---------------------------------------------------------------------------

fn expand_inherent_impl(
    item_impl: ItemImpl,
    state_ty: Type,
    key_ty: Option<Type>,
) -> syn::Result<TokenStream> {
    let mut process_item_fn: Option<&ImplItemFn> = None;
    let mut has_open = false;
    let mut has_flush_window = false;
    let mut has_on_close = false;
    let mut has_window = false;
    let mut has_on_item_error = false;
    let mut has_config = false;
    let mut has_name = false;
    let mut has_item_ty = false;
    let mut has_output_ty = false;
    let mut has_cursor_ty = false;
    let mut errors: Vec<syn::Error> = Vec::new();

    for it in &item_impl.items {
        match it {
            ImplItem::Fn(f) => match f.sig.ident.to_string().as_str() {
                "open" => has_open = true,
                "process_item" => process_item_fn = Some(f),
                "flush_window" => has_flush_window = true,
                "on_close" => has_on_close = true,
                "window" => has_window = true,
                "on_item_error" => has_on_item_error = true,
                "config" => has_config = true,
                "name" => has_name = true,
                other => {
                    errors.push(syn::Error::new_spanned(
                        &f.sig.ident,
                        format!(
                            "#[cano::task::stream]: unexpected method `{other}` in inherent impl; \
                             allowed: `open`, `process_item`, `flush_window`, `on_close`, \
                             `window`, `on_item_error`, `config`, `name`"
                        ),
                    ));
                }
            },
            ImplItem::Type(t) => match t.ident.to_string().as_str() {
                "Item" => has_item_ty = true,
                "Output" => has_output_ty = true,
                "Cursor" => has_cursor_ty = true,
                other => {
                    errors.push(syn::Error::new_spanned(
                        &t.ident,
                        format!(
                            "#[cano::task::stream]: unexpected associated type `{other}`; \
                             only `Item`, `Output`, `Cursor` are recognised (and can be inferred)"
                        ),
                    ));
                }
            },
            _ => {}
        }
    }

    if !has_open {
        errors.push(syn::Error::new(
            item_impl.span(),
            "#[cano::task::stream] requires an `async fn open(&self, res: &Resources<_>, \
             cursor: Option<_>) -> Result<Pin<Box<dyn Stream<Item = _> + Send>>, CanoError>` method",
        ));
    }
    if process_item_fn.is_none() {
        errors.push(syn::Error::new(
            item_impl.span(),
            "#[cano::task::stream] requires an `async fn process_item(&self, res: &Resources<_>, \
             item: T) -> Result<(Output, Cursor), CanoError>` method",
        ));
    }
    if !has_flush_window {
        errors.push(syn::Error::new(
            item_impl.span(),
            "#[cano::task::stream] requires an `async fn flush_window(&self, res: &Resources<_>, \
             outputs: Vec<_>) -> Result<WindowSignal<_>, CanoError>` method",
        ));
    }
    if !has_on_close {
        errors.push(syn::Error::new(
            item_impl.span(),
            "#[cano::task::stream] requires an `async fn on_close(&self, res: &Resources<_>, \
             reason: CloseReason) -> Result<TaskResult<_>, CanoError>` method",
        ));
    }

    if !errors.is_empty() {
        return Err(combine_errors(errors));
    }

    let process_item = process_item_fn.unwrap();

    // Infer Item / Output / Cursor from `process_item`.
    let item_inj = if has_item_ty {
        None
    } else {
        let inferred = peel_owned_param(process_item, "process_item")?;
        Some(quote!(type Item = #inferred;))
    };
    let (output_inj, cursor_inj) = if has_output_ty && has_cursor_ty {
        (None, None)
    } else {
        let (out_ty, cur_ty) = peel_result_tuple_return(process_item, "process_item")?;
        let out = if has_output_ty {
            None
        } else {
            Some(quote!(type Output = #out_ty;))
        };
        let cur = if has_cursor_ty {
            None
        } else {
            Some(quote!(type Cursor = #cur_ty;))
        };
        (out, cur)
    };

    let stream_trait_ref: syn::Path = match &key_ty {
        Some(k) => syn::parse_quote!(::cano::StreamTask<#state_ty, #k>),
        None => syn::parse_quote!(::cano::StreamTask<#state_ty>),
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

    let window_default = (!has_window).then(|| {
        quote! {
            fn window(&self) -> ::cano::StreamWindow {
                ::cano::StreamWindow::Count(1)
            }
        }
    });
    let on_item_error_default = (!has_on_item_error).then(|| {
        quote! {
            fn on_item_error(&self) -> ::cano::StreamErrorPolicy {
                ::cano::StreamErrorPolicy::FailFast
            }
        }
    });
    // Streams default to no outer retry (like PollTask): an outer retry would
    // re-invoke `open()` and re-consume the stream.
    let config_default = (!has_config).then(|| {
        quote! {
            fn config(&self) -> ::cano::TaskConfig {
                ::cano::TaskConfig::minimal()
            }
        }
    });
    let name_default = (!has_name).then(|| {
        quote! {
            fn name(&self) -> ::std::borrow::Cow<'static, str> {
                ::std::borrow::Cow::Borrowed(::std::any::type_name::<Self>())
            }
        }
    });

    let synth = quote! {
        #(#attrs)*
        #unsafety impl #generics #stream_trait_ref for #self_ty #where_clause {
            #item_inj
            #output_inj
            #cursor_inj
            #window_default
            #on_item_error_default
            #config_default
            #name_default
            #(#user_items)*
        }
    };

    let synth_impl: ItemImpl = parse2(synth)?;
    let stream_impl = async_rewrite::rewrite_impl_block(synth_impl.clone());

    let module_prefix = ModulePrefix::Cano;
    let task_impl = synthesise_task_impl(
        &synth_impl,
        &state_ty,
        key_ty.as_ref(),
        task_trait_path,
        &module_prefix,
    )?;

    Ok(quote! {
        #stream_impl
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
                "StreamTask impl must have angle-bracketed type arguments \
                 (e.g. `StreamTask<MyState>`)",
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
            "StreamTask impl requires at least one type argument (the state type)",
        ));
    }

    let state_ty = type_args[0].clone();
    let key_ty = type_args.get(1).map(|t| (*t).clone());

    let module_prefix = derive_module_prefix(trait_path);
    let task_path = derive_task_path_from_stream_path(trait_path, &state_ty, key_ty.as_ref())?;

    Ok((state_ty, key_ty, task_path, module_prefix))
}

fn derive_task_path_from_stream_path(
    stream_path: &Path,
    state_ty: &Type,
    key_ty: Option<&Type>,
) -> syn::Result<Path> {
    let mut task_path = stream_path.clone();

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

/// Synthesise `impl Task<S [, K]> for T` whose `Task::run` forwards to
/// `StreamTask::run_in_memory`, and whose `config`/`name` forward to
/// `StreamTask::config`/`StreamTask::name` via UFCS.
///
/// `Task::run` is written as a hand-desugared `fn` (not `async fn`) so that no
/// `for<'async_trait>` binder is introduced; it simply returns the already-pinned
/// future produced by `run_in_memory`. This is uniform across all module prefixes.
fn synthesise_task_impl(
    stream_impl: &ItemImpl,
    state_ty: &Type,
    key_ty: Option<&Type>,
    task_trait_path: Path,
    module_prefix: &ModulePrefix,
) -> syn::Result<TokenStream> {
    let attrs = &stream_impl.attrs;
    let generics = &stream_impl.generics;
    let where_clause = &stream_impl.generics.where_clause;
    let self_ty = &stream_impl.self_ty;

    let (_, stream_trait_path, _) = stream_impl.trait_.as_ref().ok_or_else(|| {
        syn::Error::new(stream_impl.span(), "expected a StreamTask trait impl block")
    })?;

    let task_config_ty = module_prefix.qualify("TaskConfig");
    let resources_ty = module_prefix.qualify("Resources");
    let task_result_ty = module_prefix.qualify("TaskResult");
    let cano_error_ty = module_prefix.qualify("CanoError");

    let key_ty_tok: TokenStream = match key_ty {
        Some(k) => quote! { #k },
        None => quote! { ::std::borrow::Cow<'static, str> },
    };

    let synth = quote! {
        #(#attrs)*
        impl #generics #task_trait_path for #self_ty #where_clause {
            fn config(&self) -> #task_config_ty {
                <Self as #stream_trait_path>::config(self)
            }
            fn name(&self) -> ::std::borrow::Cow<'static, str> {
                <Self as #stream_trait_path>::name(self)
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
                <Self as #stream_trait_path>::run_in_memory(self, res)
            }
        }
    };

    parse2(synth).map(|impl_block: ItemImpl| quote! { #impl_block })
}

// ---------------------------------------------------------------------------
// Type-inference helpers
// ---------------------------------------------------------------------------

/// Return the type of the owned third parameter of `process_item`
/// (`&self`, `res`, then `item: T` → `T`). The type is returned verbatim — no
/// reference is peeled (stream items are owned, unlike `BatchTask::process_item`).
fn peel_owned_param(f: &ImplItemFn, fn_name: &str) -> syn::Result<Type> {
    let third = f.sig.inputs.iter().nth(2).ok_or_else(|| {
        syn::Error::new_spanned(
            &f.sig,
            format!(
                "#[cano::task::stream]: `{fn_name}` must have a third parameter \
                 `item: T` (an owned type) from which `type Item` can be inferred"
            ),
        )
    })?;

    match third {
        FnArg::Typed(pt) => Ok((*pt.ty).clone()),
        FnArg::Receiver(_) => Err(syn::Error::new_spanned(
            third,
            format!(
                "#[cano::task::stream]: `{fn_name}` third parameter must be a typed argument, not `self`"
            ),
        )),
    }
}

/// Extract `(Output, Cursor)` from the `Ok` 2-tuple of `process_item`'s
/// `Result<(Output, Cursor), _>` return type.
fn peel_result_tuple_return(f: &ImplItemFn, fn_name: &str) -> syn::Result<(Type, Type)> {
    let ret_ty = match &f.sig.output {
        ReturnType::Type(_, t) => &**t,
        ReturnType::Default => {
            return Err(syn::Error::new_spanned(
                &f.sig,
                format!(
                    "#[cano::task::stream]: `{fn_name}` must return \
                     `Result<(Output, Cursor), CanoError>` so `type Output` / `type Cursor` \
                     can be inferred"
                ),
            ));
        }
    };

    let Type::Path(tp) = ret_ty else {
        return Err(tuple_return_error(fn_name, ret_ty));
    };
    let last = tp
        .path
        .segments
        .last()
        .ok_or_else(|| tuple_return_error(fn_name, ret_ty))?;
    if last.ident != "Result" && last.ident != "CanoResult" {
        return Err(tuple_return_error(fn_name, ret_ty));
    }
    let PathArguments::AngleBracketed(args) = &last.arguments else {
        return Err(tuple_return_error(fn_name, ret_ty));
    };
    let ok_ty = args
        .args
        .iter()
        .find_map(|a| match a {
            GenericArgument::Type(t) => Some(t),
            _ => None,
        })
        .ok_or_else(|| tuple_return_error(fn_name, ret_ty))?;

    let Type::Tuple(tuple) = ok_ty else {
        return Err(tuple_return_error(fn_name, ret_ty));
    };
    if tuple.elems.len() != 2 {
        return Err(tuple_return_error(fn_name, ret_ty));
    }
    let mut it = tuple.elems.iter();
    let output_ty = it.next().unwrap().clone();
    let cursor_ty = it.next().unwrap().clone();
    Ok((output_ty, cursor_ty))
}

fn tuple_return_error(fn_name: &str, ret_ty: &Type) -> syn::Error {
    syn::Error::new_spanned(
        ret_ty,
        format!(
            "#[cano::task::stream]: `{fn_name}` must return \
             `Result<(Output, Cursor), CanoError>` (a 2-tuple `Ok` type) so that \
             `type Output` / `type Cursor` can be inferred; found `{}`. Write explicit \
             `type Output = ..;` / `type Cursor = ..;` lines if the return shape differs.",
            quote! { #ret_ty }
        ),
    )
}
