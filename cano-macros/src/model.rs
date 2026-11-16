//! Shared driver for the specialized task-model attribute macros
//! (`#[task::router]`, `#[task::poll]`, `#[task::timer]`, `#[task::batch]`,
//! `#[task::stepped]`, `#[task::stream]`).
//!
//! Each model is described by a [`Model`] constant; [`expand`] handles every
//! surface form the same way:
//!
//! 1. **Trait definition** — async rewrite only.
//! 2. **`impl Trait for T` for an unrelated trait** — async rewrite only (the
//!    attribute doubles as a plain async-fn-in-trait rewriter).
//! 3. **`impl {Model}<S> for T`** — async rewrite plus the companion
//!    `impl Task<S> for T`.
//! 4. **Inherent `impl T { ... }`** with `state = S` — the driver
//!    validates the method set, infers omitted associated types, builds the
//!    `impl ::cano::{Model}<S> for T` header, and emits the same companion.
//!
//! ## Why a companion `impl Task` (not a blanket impl)
//!
//! A blanket `impl<P: PollTask<..>> Task<..> for P` would conflict (E0119) with the
//! analogous blanket impls for the other model traits — a type can implement more
//! than one — so the companion is generated per use site.
//!
//! ## Why the companion `run` is hand-desugared
//!
//! Generating `async fn run` and piping it through the async rewriter wraps the body
//! in `Box::pin(async move { ... })` under a `for<'async_trait>` binder; awaiting a
//! generic driver such as `run_batch(self, res)` inside it then demands
//! `for<'0> BatchTask<S, Cow<'0, str>>` ("implementation is not general enough").
//! Returning `Box::pin(driver(self, res))` from a hand-written signature produces one
//! future state machine and keeps the key type monomorphic.

use proc_macro2::TokenStream;
use quote::{format_ident, quote};
use syn::spanned::Spanned;
use syn::{
    GenericArgument, ImplItem, ImplItemFn, ItemImpl, ItemTrait, PathArguments, Type, parse2,
};

use crate::async_rewrite;
use crate::attr_args::{AttrArgs, combine_errors};

/// Static description of one task model.
pub(crate) struct Model {
    /// Attribute path without the crate prefix (`"task::poll"`); diagnostics print
    /// `#[cano::{attr}]`.
    pub attr: &'static str,
    /// Trait name (`"PollTask"`).
    pub trait_ident: &'static str,
    /// Methods the inherent form must define.
    pub required: &'static [&'static str],
    /// Methods the inherent form may define.
    pub optional: &'static [&'static str],
    /// Associated types the inherent form may spell.
    pub assoc: &'static [&'static str],
    /// Methods whose trait signature takes `res: &Resources` first: `(name, number of
    /// non-receiver parameters the trait declares)`. A user method with exactly one
    /// parameter fewer gets `_res: &::cano::Resources` inserted, so resource-free code
    /// can omit it.
    pub res_first: &'static [(&'static str, usize)],
    /// Inherent form: `type X = …;` items for associated types the user omitted.
    pub infer_assoc: fn(&ItemImpl) -> syn::Result<Vec<ImplItem>>,
    /// Expression producing the companion `Task::run` boxed future; `None` = no
    /// companion impl.
    pub companion_run: Option<fn(state: &Type) -> TokenStream>,
    /// `StateEntry` constructor the emitted `Registrable` impl calls (`"single"`,
    /// `"router"`, `"stepped"`, `"stream"`, `"compensatable"`).
    pub entry: &'static str,
}

/// `::cano::{ident}<S>`.
pub(crate) fn trait_path(ident: &str, state: &Type) -> TokenStream {
    let ident = format_ident!("{ident}");
    quote!(::cano::#ident<#state>)
}

/// Find the method `name` in an impl block.
pub(crate) fn find_fn<'a>(item: &'a ItemImpl, name: &str) -> Option<&'a ImplItemFn> {
    item.items.iter().find_map(|it| match it {
        ImplItem::Fn(f) if f.sig.ident == name => Some(f),
        _ => None,
    })
}

/// Whether the impl block spells the associated type `name`.
pub(crate) fn has_type(item: &ItemImpl, name: &str) -> bool {
    item.items
        .iter()
        .any(|it| matches!(it, ImplItem::Type(t) if t.ident == name))
}

/// Inference hook for models without associated types.
pub(crate) fn no_assoc(_: &ItemImpl) -> syn::Result<Vec<ImplItem>> {
    Ok(Vec::new())
}

pub(crate) fn expand(
    model: &Model,
    attr: TokenStream,
    item: TokenStream,
) -> syn::Result<TokenStream> {
    let attr_name = model.attr;
    if let Ok(trait_def) = parse2::<ItemTrait>(item.clone()) {
        if !attr.is_empty() {
            return Err(syn::Error::new(
                trait_def.span(),
                format!(
                    "#[cano::{attr_name}]: no attribute args are accepted on a trait definition"
                ),
            ));
        }
        return Ok(async_rewrite::rewrite_trait(trait_def));
    }

    let Ok(item_impl) = parse2::<ItemImpl>(item) else {
        return Err(syn::Error::new(
            proc_macro2::Span::call_site(),
            format!("#[cano::{attr_name}]: expected a trait definition or impl block"),
        ));
    };
    let args = AttrArgs::parse(attr)?;

    match &item_impl.trait_ {
        Some((path, _)) => {
            if args.state.is_some() {
                return Err(syn::Error::new(
                    item_impl.span(),
                    format!(
                        "#[cano::{attr_name}]: `state` only applies to inherent \
                         `impl T {{ ... }}` blocks; when writing `impl {}<...> for T` the \
                         trait header already specifies it",
                        model.trait_ident
                    ),
                ));
            }
            let Some(last) = path.segments.last() else {
                return Ok(async_rewrite::rewrite_impl(item_impl));
            };
            if last.ident != model.trait_ident {
                return Ok(async_rewrite::rewrite_impl(item_impl));
            }
            let state = trait_state(model, &item_impl, &last.arguments)?;
            let mut item_impl = item_impl;
            insert_res_params(model, &mut item_impl);
            let companion = companion(model, &item_impl, &state);
            let registrable = registrable(model, &item_impl, &state);
            let rewritten = async_rewrite::rewrite_impl(item_impl);
            Ok(quote! { #rewritten #companion #registrable })
        }
        None => {
            let state = args.state.ok_or_else(|| {
                syn::Error::new(
                    item_impl.span(),
                    format!(
                        "#[cano::{attr_name}] on an inherent `impl T {{ ... }}` block requires \
                         `state = T` (e.g. `#[{attr_name}(state = MyState)]`)"
                    ),
                )
            })?;
            expand_inherent(model, item_impl, state)
        }
    }
}

/// Read `S` from `{Model}<S>`.
fn trait_state(
    model: &Model,
    item_impl: &ItemImpl,
    arguments: &PathArguments,
) -> syn::Result<Type> {
    let trait_ident = model.trait_ident;
    let PathArguments::AngleBracketed(args) = arguments else {
        return Err(syn::Error::new(
            item_impl.span(),
            format!(
                "{trait_ident} impl must have angle-bracketed type arguments \
                 (e.g. `{trait_ident}<MyState>`)"
            ),
        ));
    };
    args.args
        .iter()
        .find_map(|a| match a {
            GenericArgument::Type(t) => Some(t.clone()),
            _ => None,
        })
        .ok_or_else(|| {
            syn::Error::new(
                item_impl.span(),
                format!("{trait_ident} impl requires a type argument (the state type)"),
            )
        })
}

/// See [`Model::res_first`].
fn insert_res_params(model: &Model, item_impl: &mut ItemImpl) {
    for item in &mut item_impl.items {
        let ImplItem::Fn(f) = item else { continue };
        let Some(&(_, declared)) = model.res_first.iter().find(|(name, _)| f.sig.ident == name)
        else {
            continue;
        };
        let has_receiver = f.sig.receiver().is_some();
        let params = f.sig.inputs.len() - usize::from(has_receiver);
        if params + 1 == declared {
            f.sig.inputs.insert(
                usize::from(has_receiver),
                syn::parse_quote!(_res: &::cano::Resources),
            );
        }
    }
}

fn expand_inherent(
    model: &Model,
    mut item_impl: ItemImpl,
    state: Type,
) -> syn::Result<TokenStream> {
    let attr_name = model.attr;
    let mut errors = Vec::new();
    for it in &item_impl.items {
        match it {
            ImplItem::Fn(f) => {
                let name = f.sig.ident.to_string();
                if !model.required.contains(&name.as_str())
                    && !model.optional.contains(&name.as_str())
                {
                    let allowed: Vec<String> = model
                        .required
                        .iter()
                        .chain(model.optional)
                        .map(|m| format!("`{m}`"))
                        .collect();
                    errors.push(syn::Error::new_spanned(
                        &f.sig.ident,
                        format!(
                            "#[cano::{attr_name}]: unexpected method `{name}` in inherent impl; \
                             allowed: {}",
                            allowed.join(", ")
                        ),
                    ));
                }
            }
            ImplItem::Type(t) => {
                let name = t.ident.to_string();
                if !model.assoc.contains(&name.as_str()) {
                    let msg = if model.assoc.is_empty() {
                        format!(
                            "#[cano::{attr_name}]: associated types are not part of the `{}` trait",
                            model.trait_ident
                        )
                    } else {
                        let allowed: Vec<String> =
                            model.assoc.iter().map(|a| format!("`{a}`")).collect();
                        format!(
                            "#[cano::{attr_name}]: unexpected associated type `{name}`; \
                             only {} are recognised",
                            allowed.join(", ")
                        )
                    };
                    errors.push(syn::Error::new_spanned(&t.ident, msg));
                }
            }
            _ => {}
        }
    }
    for required in model.required {
        if find_fn(&item_impl, required).is_none() {
            errors.push(syn::Error::new(
                item_impl.span(),
                format!(
                    "#[cano::{attr_name}] requires an `async fn {required}(..)` method \
                     (see the `{}` trait for its signature)",
                    model.trait_ident
                ),
            ));
        }
    }
    if !errors.is_empty() {
        return Err(combine_errors(errors));
    }

    insert_res_params(model, &mut item_impl);
    let inferred = (model.infer_assoc)(&item_impl)?;
    item_impl.items.splice(0..0, inferred);
    let path = trait_path(model.trait_ident, &state);
    item_impl.trait_ = Some((parse2(path)?, Default::default()));

    let companion = companion(model, &item_impl, &state);
    let registrable = registrable(model, &item_impl, &state);
    let rewritten = async_rewrite::rewrite_impl(item_impl);
    Ok(quote! { #rewritten #companion #registrable })
}

/// `impl ::cano::workflow::Registrable<S> for T`, selecting the engine path for the
/// model. `Self: 'static` keeps types with lifetime parameters compiling (they are
/// simply not registrable). The model-trait bound is written higher-ranked so it is
/// not a trivially-checked bound: an attribute on an impl of some *other* trait with
/// the same name (a local `trait Task`) still compiles, the impl is just unusable.
fn registrable(model: &Model, item_impl: &ItemImpl, state: &Type) -> TokenStream {
    let entry = format_ident!("{}", model.entry);
    let model_trait = format_ident!("{}", model.trait_ident);
    let generics = &item_impl.generics;
    let self_ty = &item_impl.self_ty;
    let predicates = item_impl
        .generics
        .where_clause
        .as_ref()
        .map(|w| &w.predicates);
    quote! {
        impl #generics ::cano::workflow::Registrable<#state> for #self_ty
        where
            Self: 'static,
            for<'__cano> Self: ::cano::#model_trait<#state>,
            #predicates
        {
            fn into_entry(self) -> ::cano::workflow::StateEntry<#state> {
                ::cano::workflow::StateEntry::#entry(self)
            }
        }
    }
}

/// `impl ::cano::Task<S> for T` forwarding `config` / `name` to the model trait and
/// returning `companion_run(..)` from a hand-desugared `run`.
fn companion(model: &Model, item_impl: &ItemImpl, state: &Type) -> TokenStream {
    let Some(run) = model.companion_run else {
        return TokenStream::new();
    };
    let model_trait = trait_path(model.trait_ident, state);
    let body = run(state);
    let attrs = &item_impl.attrs;
    let generics = &item_impl.generics;
    let where_clause = &item_impl.generics.where_clause;
    let self_ty = &item_impl.self_ty;
    quote! {
        #(#attrs)*
        impl #generics ::cano::Task<#state> for #self_ty #where_clause {
            fn config(&self) -> ::cano::TaskConfig {
                <Self as #model_trait>::config(self)
            }
            fn name(&self) -> ::std::borrow::Cow<'static, str> {
                <Self as #model_trait>::name(self)
            }
            fn run<'life0, 'life1, 'async_trait>(
                &'life0 self,
                res: &'life1 ::cano::Resources,
            ) -> ::core::pin::Pin<::std::boxed::Box<
                dyn ::core::future::Future<
                    Output = ::std::result::Result<#state, ::cano::CanoError>
                > + ::core::marker::Send + 'async_trait
            >>
            where
                'life0: 'async_trait,
                'life1: 'async_trait,
                Self: ::core::marker::Sync + 'async_trait,
            {
                #body
            }
        }
    }
}
