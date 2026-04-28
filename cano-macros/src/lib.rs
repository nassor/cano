//! # cano-macros
//!
//! Procedural macros backing the [`cano`](https://docs.rs/cano) workflow engine.
//!
//! The crate exposes one attribute macro per `cano` core trait, all of which
//! perform the same async-fn-to-`Pin<Box<dyn Future + Send>>` rewrite that the
//! `async-trait` crate does. Splitting the macro by trait name (rather than a
//! single generic `async_trait`) gives a more self-documenting attribute at the
//! developer side: `#[cano::task]` on `impl Task` is immediately scannable for
//! what the impl is.
//!
//! Available macros:
//!
//! - [`task`] — for `impl Task` (and the `Task` trait definition itself)
//! - [`node`] — for `impl Node` and the `Node` trait
//! - [`resource`] — for `impl Resource` and the `Resource` trait
//!
//! All three are functionally identical; they differ only in name. New traits
//! that need async-fn-in-dyn rewriting can ship their own `cano-macros`
//! attribute alongside.

use proc_macro::TokenStream;

mod async_rewrite;
mod attr_args;
mod from_resources;
mod node_impl;
mod resource_derive;
mod task_impl;

/// Derive a `from_resources(&Resources<_>) -> CanoResult<Self>` constructor that
/// pulls each field out of a `cano::Resources` map.
///
/// Each field must be `Arc<T>`. Use `#[res("key")]` for string-literal lookups
/// or `#[res(EnumType::Variant)]` for enum-path lookups. Use
/// `#[from_resources(key = MyType)]` on the struct to override the inferred key type.
///
/// # Example
///
/// ```ignore
/// use cano::prelude::*;
/// use std::sync::Arc;
///
/// #[derive(FromResources)]
/// struct Deps {
///     #[res("store")]
///     store: Arc<MemoryStore>,
/// }
/// ```
#[proc_macro_derive(FromResources, attributes(res, from_resources))]
pub fn derive_from_resources(input: TokenStream) -> TokenStream {
    from_resources::expand(input.into())
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

/// Apply to `impl Task for ...` blocks (or the `Task` trait definition itself).
///
/// Rewrites every `async fn` method into a method returning
/// `Pin<Box<dyn Future<Output = ...> + Send + 'async_trait>>`, the same shape
/// `async-trait` produces. This makes the methods callable through
/// `dyn Task<...>`.
///
/// Two surface forms are supported:
///
/// 1. **Trait-impl form (legacy):** `#[task] impl Task<S> for X { ... }` — user
///    writes the trait header.
/// 2. **Inherent-impl form:** `#[task(state = S [, key = K])] impl X { ... }` —
///    user writes only the inherent block; the macro builds the trait header
///    and enforces that exactly one of `run` / `run_bare` is present.
///
/// # Example
///
/// ```ignore
/// use cano::task;
///
/// #[task(state = MyState)]
/// impl MyTask {
///     async fn run_bare(&self) -> Result<TaskResult<MyState>, CanoError> {
///         Ok(TaskResult::Single(MyState::Done))
///     }
/// }
/// ```
#[proc_macro_attribute]
pub fn task(attr: TokenStream, item: TokenStream) -> TokenStream {
    // Try the inherent-impl path when attr args are present, OR when the impl
    // block is an inherent (non-trait) impl. Otherwise fall through to the
    // plain async rewriter.
    if let Ok(item_impl) = syn::parse::<syn::ItemImpl>(item.clone()) {
        let attr2: proc_macro2::TokenStream = attr.into();
        let has_attr = !attr2.is_empty();
        let is_inherent = item_impl.trait_.is_none();
        if has_attr || is_inherent {
            return task_impl::expand(attr2, item.into())
                .unwrap_or_else(syn::Error::into_compile_error)
                .into();
        }
    }
    async_rewrite::rewrite(item)
}

/// Apply to `impl Node for ...` blocks, inherent `impl X { ... }` blocks, or
/// the `Node` trait definition itself.
///
/// Two surface forms are supported on impl blocks:
///
/// 1. **Trait-impl form (legacy):** `#[node] impl Node<S> for X { ... }`. The
///    macro infers `type PrepResult` / `type ExecResult` from the return types
///    of `prep` and `exec`, and supplies a default `fn config(&self) -> TaskConfig`
///    when missing.
/// 2. **Inherent-impl form:** `#[node(state = S [, key = K])] impl X { ... }`.
///    The macro builds the `impl Node<S [, K]> for X` header from the attribute
///    args, enforces that `prep` / `exec` / `post` are present, and injects
///    the same boilerplate as form 1.
///
/// On a trait definition (`#[node] pub trait Node ...`) the macro just performs
/// the async-fn-in-trait rewrite.
#[proc_macro_attribute]
pub fn node(attr: TokenStream, item: TokenStream) -> TokenStream {
    if let Ok(item_impl) = syn::parse::<syn::ItemImpl>(item.clone()) {
        let attr2: proc_macro2::TokenStream = attr.into();
        let has_attr = !attr2.is_empty();
        let is_inherent = item_impl.trait_.is_none();

        // Inherent form: always go through the boilerplate filler (it builds
        // the trait header from attr args and enforces mandatory methods).
        if is_inherent || has_attr {
            return node_impl::expand(attr2, item.into())
                .unwrap_or_else(syn::Error::into_compile_error)
                .into();
        }

        // Trait-impl form: only run the boilerplate filler when inference is
        // actually needed (at least one of PrepResult/ExecResult is absent).
        let has_prep = item_impl
            .items
            .iter()
            .any(|it| matches!(it, syn::ImplItem::Type(t) if t.ident == "PrepResult"));
        let has_exec = item_impl
            .items
            .iter()
            .any(|it| matches!(it, syn::ImplItem::Type(t) if t.ident == "ExecResult"));
        if !has_prep || !has_exec {
            return node_impl::expand(proc_macro2::TokenStream::new(), item.into())
                .unwrap_or_else(syn::Error::into_compile_error)
                .into();
        }
    }
    async_rewrite::rewrite(item)
}

/// Apply to `impl Resource for ...` blocks (or the `Resource` trait definition itself).
///
/// Rewrites every `async fn` method into a method returning
/// `Pin<Box<dyn Future<Output = ...> + Send + 'async_trait>>`. Behaviorally
/// identical to [`task`] and [`node`]; the separate name makes the attribute
/// self-documenting at impl sites.
#[proc_macro_attribute]
pub fn resource(_attr: TokenStream, item: TokenStream) -> TokenStream {
    async_rewrite::rewrite(item)
}

/// Derive an empty `cano::Resource` impl (uses the trait's default no-op
/// `setup` / `teardown`).
///
/// Apply this derive to any struct that needs to implement `Resource` but has no
/// custom lifecycle logic. The trait's `setup` and `teardown` defaults (which
/// return `Ok(())`) take effect automatically.
///
/// # Example
///
/// ```ignore
/// use cano::prelude::*;
///
/// #[derive(Resource)]
/// struct MyConfig {
///     timeout_ms: u64,
/// }
/// ```
#[proc_macro_derive(Resource)]
pub fn derive_resource(input: TokenStream) -> TokenStream {
    resource_derive::expand(input.into())
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}
