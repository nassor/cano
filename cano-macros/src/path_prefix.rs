//! Shared `ModulePrefix` helper — used by all task-trait macro impls to resolve
//! how cano types are referenced in the synthesised companion `impl Task` blocks.
//!
//! The four task macros (`router_task_impl`, `poll_task_impl`, `batch_task_impl`,
//! `stepped_task_impl`) all need to emit paths like `::cano::TaskConfig` or
//! bare `TaskConfig` depending on whether the user wrote `::cano::XxxTask<S>`,
//! `cano::XxxTask<S>`, or just `XxxTask<S>` in their impl header.  This module
//! centralises that logic so it lives in one place.

use proc_macro2::TokenStream;
use quote::quote;
use syn::{Path, PathSegment};

// ---------------------------------------------------------------------------
// ModulePrefix
// ---------------------------------------------------------------------------

/// The path prefix to use when referencing cano types in the companion `impl Task`.
///
/// - `Cano` → `::cano::TypeName` (for the inherent form or external users)
/// - `Prefixed(path)` → `prefix::TypeName` (e.g. `cano::TypeName`)
/// - `Bare` → unqualified `TypeName` (for the trait-impl form inside the cano crate itself,
///   where types are in scope via `use super::*` or similar imports)
pub(crate) enum ModulePrefix {
    /// Use `::cano::TypeName`.
    Cano,
    /// Use `prefix::TypeName` where prefix is the path before the last segment.
    Prefixed(TokenStream),
    /// Use bare `TypeName` (name only, no prefix).
    Bare,
}

impl ModulePrefix {
    /// Build a qualified path token stream: `{prefix}TypeName`.
    pub(crate) fn qualify(&self, type_name: &str) -> TokenStream {
        let ident = syn::Ident::new(type_name, proc_macro2::Span::call_site());
        match self {
            ModulePrefix::Cano => quote! { ::cano::#ident },
            ModulePrefix::Prefixed(prefix) => quote! { #prefix #ident },
            ModulePrefix::Bare => quote! { #ident },
        }
    }
}

// ---------------------------------------------------------------------------
// Constructor helpers
// ---------------------------------------------------------------------------

/// Derive a [`ModulePrefix`] from any trait path (e.g. `::cano::PollTask<S>`).
///
/// The function is generic over the trait name — pass any path that ends in the
/// trait segment; only the *prefix* (all segments except the last) matters.
///
/// - `XxxTask<S>` (no prefix, no leading colon) → [`ModulePrefix::Bare`]
/// - `::cano::XxxTask<S>` (leading colon + single `cano` segment) → [`ModulePrefix::Cano`]
/// - anything else → [`ModulePrefix::Prefixed`]
pub(crate) fn derive_module_prefix(trait_path: &Path) -> ModulePrefix {
    let seg_count = trait_path.segments.len();

    if seg_count <= 1 && trait_path.leading_colon.is_none() {
        // No prefix (bare `XxxTask<S>`).
        return ModulePrefix::Bare;
    }

    // Collect all segments except the last one as the prefix.
    let prefix_segs: syn::punctuated::Punctuated<PathSegment, syn::token::PathSep> = trait_path
        .segments
        .iter()
        .take(seg_count.saturating_sub(1))
        .cloned()
        .collect();

    // Check if this is `::cano::` (leading colon + single "cano" prefix segment).
    let has_leading_colon = trait_path.leading_colon.is_some();
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
