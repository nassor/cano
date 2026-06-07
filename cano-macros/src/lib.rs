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
//! These macros are intended to be used via their namespaced paths in `cano`:
//!
//! - `#[cano::task]` — for `impl Task` (and the `Task` trait definition itself)
//! - `#[cano::task::router]` — for `impl RouterTask` and the `RouterTask` trait
//! - `#[cano::task::poll]` — for `impl PollTask` and the `PollTask` trait
//! - `#[cano::task::timer]` — for `impl TimerTask` and the `TimerTask` trait
//! - `#[cano::task::batch]` — for `impl BatchTask` and the `BatchTask` trait
//! - `#[cano::task::stepped]` — for `impl SteppedTask` and the `SteppedTask` trait
//! - `#[cano::task::stream]` — for `impl StreamTask` and the `StreamTask` trait
//! - `#[cano::saga::task]` — for `impl CompensatableTask`
//! - `#[cano::resource]` — for `impl Resource` and the `Resource` trait
//! - `#[cano::checkpoint_store]` — for `impl CheckpointStore` and the `CheckpointStore` trait
//!
//! All are functionally identical in the async-rewrite they perform; they differ
//! only in name. New traits that need async-fn-in-dyn rewriting can ship their
//! own `cano-macros` attribute alongside.

use proc_macro::TokenStream;

mod async_rewrite;
mod attr_args;
mod batch_task_impl;
mod checkpoint_store_impl;
mod compensatable_task_impl;
mod from_resources;
mod path_prefix;
mod poll_task_impl;
mod resource_derive;
mod router_task_impl;
mod stepped_task_impl;
mod stream_task_impl;
mod task_impl;
mod timer_task_impl;

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
/// Use as `#[cano::task]`.
///
/// Two surface forms are supported:
///
/// 1. **Trait-impl form:** `#[task] impl Task<S> for X { ... }` — user
///    writes the trait header.
/// 2. **Inherent-impl form:** `#[task(state = S [, key = K])] impl X { ... }` —
///    user writes only the inherent block; the macro builds the trait header
///    and enforces that exactly one of `run` / `run_bare` is present.
///
/// For compensatable (saga) tasks, use [`#[cano::saga::task]`](compensatable_task)
/// instead.
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

/// Apply to `impl Resource for ...` blocks (or the `Resource` trait definition itself).
///
/// Rewrites every `async fn` method into a method returning
/// `Pin<Box<dyn Future<Output = ...> + Send + 'async_trait>>`. Behaviorally
/// identical to [`task`]; the separate name makes the attribute
/// self-documenting at impl sites.
#[proc_macro_attribute]
pub fn resource(_attr: TokenStream, item: TokenStream) -> TokenStream {
    async_rewrite::rewrite(item)
}

/// Apply to the `CheckpointStore` trait definition, an
/// `impl CheckpointStore for T` block, or — for less boilerplate — an inherent
/// `impl T { ... }` block.
///
/// Two surface forms on impl blocks:
///
/// 1. **Trait-impl form:** `#[checkpoint_store] impl CheckpointStore for T { ... }` —
///    user writes the trait header.
/// 2. **Inherent-impl form:** `#[checkpoint_store] impl T { async fn append(..); async fn
///    load_run(..); async fn clear(..); }` — the macro builds the `impl CheckpointStore for T`
///    header and enforces that all three methods are present. (`CheckpointStore` takes no type
///    parameters, so there are no attribute args.)
///
/// Either way, every `async fn` is rewritten to return
/// `Pin<Box<dyn Future<Output = ...> + Send + 'async_trait>>`. On the trait definition the macro
/// just performs that rewrite.
#[proc_macro_attribute]
pub fn checkpoint_store(_attr: TokenStream, item: TokenStream) -> TokenStream {
    if let Ok(item_impl) = syn::parse::<syn::ItemImpl>(item.clone())
        && item_impl.trait_.is_none()
    {
        return checkpoint_store_impl::expand(item.into())
            .unwrap_or_else(syn::Error::into_compile_error)
            .into();
    }
    async_rewrite::rewrite(item)
}

/// Apply to the `CompensatableTask` trait definition, an
/// `impl CompensatableTask<S [, K]> for T` block, or — for less boilerplate — an
/// inherent `impl T { ... }` block.
///
/// Use as `#[cano::saga::task]`.
///
/// Two surface forms on impl blocks:
///
/// 1. **Trait-impl form:** `#[saga::task] impl CompensatableTask<S> for T { type Output =
///    O; async fn run(..); async fn compensate(..); }` — user writes the trait header.
/// 2. **Inherent-impl form:** `#[saga::task(state = S [, key = K])] impl T { type Output =
///    O; async fn run(..); async fn compensate(..); }` — the macro builds the
///    `impl CompensatableTask<S [, K]> for T` header from the attribute args and enforces that
///    `type Output`, `run`, and `compensate` are present (`config` / `name` may be overridden).
///
/// Either way, every `async fn` is rewritten to return
/// `Pin<Box<dyn Future<Output = ...> + Send + 'async_trait>>`. On the trait definition the macro
/// just performs that rewrite.
#[proc_macro_attribute]
pub fn compensatable_task(attr: TokenStream, item: TokenStream) -> TokenStream {
    if let Ok(item_impl) = syn::parse::<syn::ItemImpl>(item.clone()) {
        let attr2: proc_macro2::TokenStream = attr.into();
        let is_inherent = item_impl.trait_.is_none();
        if is_inherent || !attr2.is_empty() {
            return compensatable_task_impl::expand(attr2, item.into())
                .unwrap_or_else(syn::Error::into_compile_error)
                .into();
        }
    }
    async_rewrite::rewrite(item)
}

/// Apply to the `RouterTask` trait definition, an
/// `impl RouterTask<S [, K]> for T` block, or — for less boilerplate — an
/// inherent `impl T { ... }` block.
///
/// Use as `#[cano::task::router]`.
///
/// Two surface forms on impl blocks:
///
/// 1. **Trait-impl form:** `#[task::router] impl RouterTask<S> for T { async fn route(..) { ... } }` —
///    user writes the trait header. The macro async-rewrites the `RouterTask` impl AND emits a
///    companion `impl Task<S> for T` that delegates `Task::run` → `RouterTask::route`.
/// 2. **Inherent-impl form:** `#[task::router(state = S [, key = K])] impl T { async fn route(..) { ... } }` —
///    the macro builds the `impl RouterTask<S [, K]> for T` header from the attribute args, enforces
///    that `route` is present (`config` / `name` may be overridden), and emits the same companion
///    `impl Task<S [, K]> for T`.
///
/// On a trait definition (`#[task::router] pub trait RouterTask ...`) the macro just performs the
/// async-fn-in-trait rewrite.
///
/// Because a blanket `impl<R: RouterTask<..>> Task<..> for R` would conflict (E0119) with the
/// analogous blanket impls for the other specialized task traits — a type can implement more than
/// one — the companion `Task` impl is generated per-use-site rather than as a blanket.
#[proc_macro_attribute]
pub fn router_task(attr: TokenStream, item: TokenStream) -> TokenStream {
    router_task_impl::expand(attr.into(), item.into())
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

/// Apply to the `BatchTask` trait definition, an
/// `impl BatchTask<S [, K]> for T` block, or — for less boilerplate — an
/// inherent `impl T { ... }` block.
///
/// Use as `#[cano::task::batch]`.
///
/// Two surface forms are supported on impl blocks:
///
/// 1. **Trait-impl form:** `#[task::batch] impl BatchTask<S> for T { type Item = I; type ItemOutput = O; ... }` —
///    user writes the trait header. The macro async-rewrites the `BatchTask` impl AND emits a
///    companion `impl Task<S> for T` that delegates `Task::run` → `run_batch`.
/// 2. **Inherent-impl form:** `#[task::batch(state = S [, key = K])] impl T { async fn load(..); async fn process_item(..); async fn finish(..); }` —
///    the macro builds the `impl BatchTask<S [, K]> for T` header from the attribute args,
///    infers `type Item` from the `&T` parameter of `process_item` and `type ItemOutput` from
///    its return type, enforces that `load`, `process_item`, and `finish` are present
///    (`concurrency`, `item_retry`, `config`, `name` may be overridden), and emits the same
///    companion `impl Task<S [, K]> for T`.
///
/// On a trait definition (`#[task::batch] pub trait BatchTask ...`) the macro just performs the
/// async-fn-in-trait rewrite.
///
/// Because a blanket `impl<B: BatchTask<..>> Task<..> for B` would conflict (E0119) with the
/// analogous blanket impls for the other specialized task traits — a type can implement more than
/// one — the companion `Task` impl is generated per-use-site rather than as a blanket.
///
/// `Task::run` always delegates to the `::cano::task::batch::run_batch` free function (it is
/// never inlined into the generated code) because the fan-out loop requires `futures_util`,
/// which is not a direct dependency of external callers.
#[proc_macro_attribute]
pub fn batch_task(attr: TokenStream, item: TokenStream) -> TokenStream {
    batch_task_impl::expand(attr.into(), item.into())
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

/// Apply to the `PollTask` trait definition, an
/// `impl PollTask<S [, K]> for T` block, or — for less boilerplate — an
/// inherent `impl T { ... }` block.
///
/// Use as `#[cano::task::poll]`.
///
/// Two surface forms on impl blocks:
///
/// 1. **Trait-impl form:** `#[task::poll] impl PollTask<S> for T { async fn poll(..) { ... } }` —
///    user writes the trait header. The macro async-rewrites the `PollTask` impl AND emits a
///    companion `impl Task<S> for T` that delegates `Task::run` via the `run_poll_loop` helper.
/// 2. **Inherent-impl form:** `#[task::poll(state = S [, key = K])] impl T { async fn poll(..) { ... } }` —
///    the macro builds the `impl PollTask<S [, K]> for T` header from the attribute args, enforces
///    that `poll` is present (`config` / `name` may be overridden), and emits the same companion
///    `impl Task<S [, K]> for T`.
///
/// On a trait definition (`#[task::poll] pub trait PollTask ...`) the macro just performs the
/// async-fn-in-trait rewrite.
///
/// Because a blanket `impl<P: PollTask<..>> Task<..> for P` would conflict (E0119) with the
/// analogous blanket impls for the other specialized task traits — a type can implement more than
/// one — the companion `Task` impl is generated per-use-site rather than as a blanket.
///
/// The default `config()` injected by the inherent form is [`TaskConfig::minimal()`]
/// (no retries) — the poll loop itself is the resilience mechanism.
#[proc_macro_attribute]
pub fn poll_task(attr: TokenStream, item: TokenStream) -> TokenStream {
    poll_task_impl::expand(attr.into(), item.into())
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

/// Apply to the `TimerTask` trait definition, an
/// `impl TimerTask<S [, K]> for T` block, or — for less boilerplate — an
/// inherent `impl T { ... }` block.
///
/// Use as `#[cano::task::timer]`.
///
/// Two surface forms on impl blocks:
///
/// 1. **Trait-impl form:** `#[task::timer] impl TimerTask<S> for T { async fn wait(..) { ... } async fn after_wait(..) { ... } }` —
///    user writes the trait header. The macro async-rewrites the `TimerTask` impl AND emits a
///    companion `impl Task<S> for T` whose `Task::run` sleeps for the returned `TimerOutcome`
///    (via the `run_timer` helper) then calls `after_wait`.
/// 2. **Inherent-impl form:** `#[task::timer(state = S [, key = K])] impl T { async fn wait(..) { ... } async fn after_wait(..) { ... } }` —
///    the macro builds the `impl TimerTask<S [, K]> for T` header from the attribute args, enforces
///    that both `wait` and `after_wait` are present (`config` / `name` may be overridden), and emits
///    the same companion `impl Task<S [, K]> for T`.
///
/// On a trait definition (`#[task::timer] pub trait TimerTask ...`) the macro just performs the
/// async-fn-in-trait rewrite.
///
/// Because a blanket `impl<T: TimerTask<..>> Task<..> for T` would conflict (E0119) with the
/// analogous blanket impls for the other specialized task traits — a type can implement more than
/// one — the companion `Task` impl is generated per-use-site rather than as a blanket.
///
/// The default `config()` injected by the inherent form is [`TaskConfig::minimal()`]
/// (no retries) — a single scheduled sleep needs no outer retry wrapping.
///
/// The **inherent form** routes `Task::run` through `::cano::task::timer::run_timer`, so the user
/// crate needs no direct `tokio` dependency. The **trait-impl form** inlines the sleep, so a crate
/// using `#[task::timer] impl TimerTask<S> for T` must depend on `tokio` (feature `time`),
/// reachable as `::tokio` — the same requirement the sibling `#[task::poll]` macro has.
#[proc_macro_attribute]
pub fn timer_task(attr: TokenStream, item: TokenStream) -> TokenStream {
    timer_task_impl::expand(attr.into(), item.into())
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

/// Apply to the `SteppedTask` trait definition, an
/// `impl SteppedTask<S [, K]> for T` block, or — for less boilerplate — an
/// inherent `impl T { ... }` block.
///
/// Use as `#[cano::task::stepped]`.
///
/// Two surface forms on impl blocks:
///
/// 1. **Trait-impl form:** `#[task::stepped] impl SteppedTask<S> for T { type Cursor = C; async fn step(..) { ... } }` —
///    user writes the trait header. The macro async-rewrites the `SteppedTask` impl AND emits a
///    companion `impl Task<S> for T` that delegates `Task::run` via the `run_stepped` helper.
/// 2. **Inherent-impl form:** `#[task::stepped(state = S [, key = K])] impl T { async fn step(..) { ... } }` —
///    the macro builds the `impl SteppedTask<S [, K]> for T` header from the attribute args, infers
///    `type Cursor` from the `Option<C>` third parameter of `step`, enforces that `step` is present
///    (`config` / `name` may be overridden), and emits the same companion `impl Task<S [, K]> for T`.
///
/// On a trait definition (`#[task::stepped] pub trait SteppedTask ...`) the macro just performs the
/// async-fn-in-trait rewrite.
///
/// Because a blanket `impl<S: SteppedTask<..>> Task<..> for S` would conflict (E0119) with the
/// analogous blanket impls for the other specialized task traits — a type can implement more than
/// one — the companion `Task` impl is generated per-use-site rather than as a blanket.
///
/// The default `config()` injected by the inherent form is [`TaskConfig::default()`]
/// (exponential backoff with 3 retries).
#[proc_macro_attribute]
pub fn stepped_task(attr: TokenStream, item: TokenStream) -> TokenStream {
    stepped_task_impl::expand(attr.into(), item.into())
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

/// Apply to the `StreamTask` trait definition, an `impl StreamTask<S [, K]> for T`
/// block, or an inherent `impl T { ... }` block.
///
/// Use as `#[cano::task::stream]`. `StreamTask` is a genuine stream-processing model:
/// consume an `impl Stream` continuously, flush per [`WindowPolicy`] window, run until
/// the workflow's `CancellationToken` fires, and persist a resumable cursor (via
/// [`Workflow::register_stream`]). Per-item errors are governed by [`StreamErrorPolicy`].
///
/// Two surface forms on impl blocks:
///
/// 1. **Trait-impl form:** `#[task::stream] impl StreamTask<S> for T { type Item = ..; .. }`.
/// 2. **Inherent-impl form:** `#[task::stream(state = S [, key = K])] impl T { async fn open(..) .. }` —
///    the macro infers `type Item` from `process_item`'s owned `item` parameter and
///    `type Output` / `type Cursor` from the `Ok` 2-tuple of `process_item`'s return,
///    requires `open` / `process_item` / `flush_window` / `on_close`, and emits a
///    companion `impl Task<S [, K]> for T` whose `run` forwards to `StreamTask::run_in_memory`.
///
/// On a trait definition the macro just performs the async-fn-in-trait rewrite.
///
/// The default `config()` injected by the inherent form is [`TaskConfig::minimal()`]
/// (no outer retry — like `PollTask`; an outer retry would re-invoke `open()`).
#[proc_macro_attribute]
pub fn stream_task(attr: TokenStream, item: TokenStream) -> TokenStream {
    stream_task_impl::expand(attr.into(), item.into())
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
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
