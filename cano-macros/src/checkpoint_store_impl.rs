//! Boilerplate-filling pass for `#[cano::checkpoint_store]` and
//! `#[cano::scheduler_store]` on inherent `impl T { ... }` blocks.
//!
//! `#[checkpoint_store] impl T { async fn append(..); async fn load_run(..); async fn clear(..); }`
//! expands to `impl ::cano::CheckpointStore for T { ... }` with every `async fn`
//! rewritten to return `Pin<Box<dyn Future + Send>>`. `#[scheduler_store]` does
//! the same for `::cano::SchedulerStore` (`load`, `compare_and_swap`, `remove`).
//! Neither trait has type parameters, so there are no attribute args.
//!
//! The explicit `impl Trait for T { ... }` and `trait Trait { ... }` forms go
//! through the plain async rewriter in `lib.rs`.

use std::collections::HashSet;

use proc_macro2::TokenStream;
use quote::{format_ident, quote};
use syn::{ImplItem, ItemImpl, parse2, spanned::Spanned};

use crate::attr_args::combine_errors;

pub(crate) struct StoreTraitSpec {
    pub attr_name: &'static str,
    pub trait_ident: &'static str,
    /// `(method_name, display_signature)`
    pub required: &'static [(&'static str, &'static str)],
}

pub(crate) const CHECKPOINT_STORE: StoreTraitSpec = StoreTraitSpec {
    attr_name: "checkpoint_store",
    trait_ident: "CheckpointStore",
    required: &[
        (
            "append",
            "append(&self, workflow_id: &str, row: CheckpointRow) -> Result<(), CanoError>",
        ),
        (
            "load_run",
            "load_run(&self, workflow_id: &str) -> Result<Vec<CheckpointRow>, CanoError>",
        ),
        (
            "clear",
            "clear(&self, workflow_id: &str) -> Result<(), CanoError>",
        ),
    ],
};

pub(crate) const SCHEDULER_STORE: StoreTraitSpec = StoreTraitSpec {
    attr_name: "scheduler_store",
    trait_ident: "SchedulerStore",
    required: &[
        (
            "load",
            "load(&self, flow_id: &str) -> Result<Option<VersionedRecord>, CanoError>",
        ),
        (
            "compare_and_swap",
            "compare_and_swap(&self, flow_id: &str, expected: Option<u64>, value: Vec<u8>) -> Result<CasOutcome, CanoError>",
        ),
        (
            "remove",
            "remove(&self, flow_id: &str) -> Result<(), CanoError>",
        ),
    ],
};

pub(crate) fn expand(
    spec: &StoreTraitSpec,
    attr: TokenStream,
    item: TokenStream,
) -> syn::Result<TokenStream> {
    if !attr.is_empty() {
        return Err(syn::Error::new_spanned(
            attr,
            format!(
                "#[cano::{}] does not accept attribute arguments",
                spec.attr_name
            ),
        ));
    }
    let item_impl: ItemImpl = parse2(item)?;
    debug_assert!(
        item_impl.trait_.is_none(),
        "caller routes only inherent impls here"
    );

    let mut seen_methods: HashSet<String> = HashSet::new();
    let mut errors: Vec<syn::Error> = Vec::new();
    let allowed_names: Vec<String> = spec
        .required
        .iter()
        .map(|(n, _)| format!("`{n}`"))
        .collect();

    for it in &item_impl.items {
        match it {
            ImplItem::Fn(f) => {
                let name = f.sig.ident.to_string();
                if !spec.required.iter().any(|(req, _)| *req == name) {
                    errors.push(syn::Error::new_spanned(
                        &f.sig.ident,
                        format!(
                            "#[cano::{}]: unexpected method `{name}` in inherent impl; \
                             the trait has {}",
                            spec.attr_name,
                            allowed_names.join(", ")
                        ),
                    ));
                } else if !seen_methods.insert(name.clone()) {
                    errors.push(syn::Error::new_spanned(
                        &f.sig.ident,
                        format!("#[cano::{}]: duplicate method `{name}`", spec.attr_name),
                    ));
                }
            }
            ImplItem::Type(t) => errors.push(syn::Error::new_spanned(
                &t.ident,
                format!(
                    "#[cano::{}]: the `{}` trait has no associated types",
                    spec.attr_name, spec.trait_ident
                ),
            )),
            other => errors.push(syn::Error::new_spanned(
                other,
                format!(
                    "#[cano::{}]: unsupported item in inherent impl; only trait methods are allowed",
                    spec.attr_name
                ),
            )),
        }
    }

    for (name, sig) in spec.required {
        if !seen_methods.contains(*name) {
            errors.push(syn::Error::new(
                item_impl.span(),
                format!(
                    "#[cano::{}] requires `async fn {sig}` — missing `{name}`",
                    spec.attr_name
                ),
            ));
        }
    }
    if !errors.is_empty() {
        return Err(combine_errors(errors));
    }

    let attrs = &item_impl.attrs;
    let unsafety = &item_impl.unsafety;
    let generics = &item_impl.generics;
    let where_clause = &item_impl.generics.where_clause;
    let self_ty = &item_impl.self_ty;
    let user_items = &item_impl.items;
    let trait_ident = format_ident!("{}", spec.trait_ident);

    let synth = quote! {
        #(#attrs)*
        #unsafety impl #generics ::cano::#trait_ident for #self_ty #where_clause {
            #(#user_items)*
        }
    };

    let synth_impl: ItemImpl = parse2(synth)?;
    crate::async_rewrite::rewrite_impl(synth_impl)
}
