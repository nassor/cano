//! Implementation of `#[derive(FromResources)]`.
//!
//! Generates a `Self::from_resources(res)` associated function that pulls every
//! field out of a `cano::Resources<_>` map.
//!
//! # Field attributes
//!
//! - `#[res("string_key")]` — look up the resource by a string-literal key.
//! - `#[res(EnumType::Variant)]` — look up by an enum-path key.
//!
//! # Container attribute
//!
//! - `#[from_resources(key = MyKeyType)]` — override the inferred key type.
//!
//! # Constraints
//!
//! Every field must be `Arc<T>`. Mixed key styles (some string, some path)
//! without a `#[from_resources(key = ...)]` override are rejected at compile
//! time.

use proc_macro2::TokenStream;
use quote::quote;
use syn::{
    Data, DataStruct, DeriveInput, Expr, Field, Fields, GenericArgument, Lit, Meta, Path,
    PathArguments, Type, parse2, spanned::Spanned,
};

pub(crate) fn expand(input: TokenStream) -> syn::Result<TokenStream> {
    let derive_input: DeriveInput = parse2(input)?;
    let struct_name = &derive_input.ident;
    let (impl_generics, ty_generics, where_clause) = derive_input.generics.split_for_impl();

    let fields = collect_fields(&derive_input)?;

    if fields.is_empty() {
        return Err(syn::Error::new(
            derive_input.ident.span(),
            "FromResources requires at least one field",
        ));
    }

    // Detect whether the struct uses string-literal keys, path-syntax keys, or
    // a mix; user can override the key type with #[from_resources(key = MyKey)].
    let key_override = parse_key_override(&derive_input)?;

    let any_string_key = fields.iter().any(|f| matches!(f.key, KeySource::Lit(_)));
    let any_path_key = fields.iter().any(|f| matches!(f.key, KeySource::Path(_)));

    if any_string_key && any_path_key && key_override.is_none() {
        return Err(syn::Error::new(
            derive_input.ident.span(),
            "FromResources: cannot mix string-literal and path-syntax keys; \
             use one form for all fields, or set #[from_resources(key = ...)] explicitly",
        ));
    }

    // Build the field assignments.
    let field_assigns = fields.iter().map(|field| {
        let name = field.ident;
        let inner_ty = &field.inner_ty;
        match &field.key {
            KeySource::Lit(lit) => quote! {
                #name: __res.get::<#inner_ty, str>(#lit)?
            },
            KeySource::Path(path) => quote! {
                #name: __res.get::<#inner_ty, _>(&#path)?
            },
        }
    });

    // Choose the from_resources signature based on key style + override.
    let from_resources_method = if any_path_key || (any_string_key && key_override.is_some()) {
        // Concrete key type: either all path keys or string keys with an explicit override.
        let key_ty: Type = if let Some(ty) = key_override {
            ty
        } else {
            // Infer key type from the leading path of the first path-key field.
            let first_path = fields
                .iter()
                .find_map(|f| match &f.key {
                    KeySource::Path(p) => Some(p.clone()),
                    _ => None,
                })
                .expect("any_path_key implies at least one path-syntax key");
            // Strip the trailing variant segment to get the enum/type itself.
            // e.g. `Key::Store` -> `Key`. If only one segment, error.
            let mut segments = first_path.segments.clone();
            if segments.len() > 1 {
                segments.pop();
                let segments = segments
                    .into_pairs()
                    .map(|p| p.into_value())
                    .collect::<syn::punctuated::Punctuated<_, _>>();
                let new_path = syn::Path {
                    leading_colon: first_path.leading_colon,
                    segments,
                };
                syn::parse_quote!(#new_path)
            } else {
                return Err(syn::Error::new(
                    first_path.span(),
                    "FromResources: cannot infer key type from a single-segment path; \
                     add #[from_resources(key = MyKey)] to the struct",
                ));
            }
        };

        quote! {
            pub fn from_resources(
                __res: &::cano::Resources<#key_ty>,
            ) -> ::cano::CanoResult<Self> {
                Ok(Self {
                    #(#field_assigns,)*
                })
            }
        }
    } else {
        // String-literal keys only (no override): emit a generic impl that works
        // for any TResourceKey that borrows as str.
        quote! {
            pub fn from_resources<__K>(
                __res: &::cano::Resources<__K>,
            ) -> ::cano::CanoResult<Self>
            where
                __K: ::core::borrow::Borrow<str>
                    + ::core::hash::Hash
                    + ::core::cmp::Eq
                    + ::core::marker::Send
                    + ::core::marker::Sync
                    + 'static,
            {
                Ok(Self {
                    #(#field_assigns,)*
                })
            }
        }
    };

    Ok(quote! {
        #[automatically_derived]
        impl #impl_generics #struct_name #ty_generics #where_clause {
            #from_resources_method
        }
    })
}

// ── Internal types ─────────────────────────────────────────────────────────

struct ResolvedField<'a> {
    ident: &'a syn::Ident,
    inner_ty: Type,
    key: KeySource,
}

enum KeySource {
    Lit(syn::LitStr),
    Path(Path),
}

// ── Field collection ────────────────────────────────────────────────────────

fn collect_fields(input: &DeriveInput) -> syn::Result<Vec<ResolvedField<'_>>> {
    let Data::Struct(DataStruct { fields, .. }) = &input.data else {
        return Err(syn::Error::new(
            input.ident.span(),
            "FromResources can only be derived for structs with named fields",
        ));
    };

    let Fields::Named(named) = fields else {
        return Err(syn::Error::new(
            input.ident.span(),
            "FromResources requires named fields",
        ));
    };

    let mut out = Vec::with_capacity(named.named.len());
    for field in &named.named {
        if has_skip_attr(field)? {
            return Err(syn::Error::new(
                field.span(),
                "FromResources: #[res(skip)] is not yet supported; \
                 remove the field or unmark it",
            ));
        }

        let key = parse_field_key(field)?;
        let inner_ty = extract_arc_inner(&field.ty).ok_or_else(|| {
            syn::Error::new_spanned(
                &field.ty,
                "FromResources: every field must be `Arc<T>` \
                 (it matches what `Resources::get` returns)",
            )
        })?;

        out.push(ResolvedField {
            ident: field.ident.as_ref().expect("named field"),
            inner_ty,
            key,
        });
    }

    Ok(out)
}

// ── Attribute parsers ───────────────────────────────────────────────────────

/// Look for `#[res("name")]` or `#[res(Key::Variant)]` on a field.
fn parse_field_key(field: &Field) -> syn::Result<KeySource> {
    let mut found: Option<KeySource> = None;
    for attr in &field.attrs {
        if !attr.path().is_ident("res") {
            continue;
        }

        let Meta::List(list) = &attr.meta else {
            return Err(syn::Error::new_spanned(
                attr,
                "expected #[res(\"name\")] or #[res(Path::Variant)]",
            ));
        };

        let expr: Expr = syn::parse2(list.tokens.clone())?;
        let key = match expr {
            Expr::Lit(syn::ExprLit {
                lit: Lit::Str(s), ..
            }) => KeySource::Lit(s),
            Expr::Path(syn::ExprPath { path, .. }) => KeySource::Path(path),
            other => {
                return Err(syn::Error::new_spanned(
                    other,
                    "#[res(...)] must be a string literal or a path expression \
                     (e.g. Key::Variant)",
                ));
            }
        };

        if found.is_some() {
            return Err(syn::Error::new_spanned(
                attr,
                "duplicate #[res(...)] attribute on field",
            ));
        }
        found = Some(key);
    }

    found.ok_or_else(|| {
        syn::Error::new_spanned(
            field,
            "FromResources: every field must have #[res(\"name\")] or #[res(Path::Variant)]",
        )
    })
}

fn has_skip_attr(field: &Field) -> syn::Result<bool> {
    for attr in &field.attrs {
        if !attr.path().is_ident("res") {
            continue;
        }
        if let Meta::List(list) = &attr.meta {
            let tokens = list.tokens.to_string();
            if tokens.trim() == "skip" {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

/// Look for `#[from_resources(key = MyType)]` on the struct.
fn parse_key_override(input: &DeriveInput) -> syn::Result<Option<Type>> {
    for attr in &input.attrs {
        if !attr.path().is_ident("from_resources") {
            continue;
        }
        let Meta::List(list) = &attr.meta else {
            return Err(syn::Error::new_spanned(
                attr,
                "expected #[from_resources(key = MyType)]",
            ));
        };

        struct KeyArg(Type);
        impl syn::parse::Parse for KeyArg {
            fn parse(input: syn::parse::ParseStream<'_>) -> syn::Result<Self> {
                let ident: syn::Ident = input.parse()?;
                if ident != "key" {
                    return Err(syn::Error::new(ident.span(), "expected `key = MyType`"));
                }
                let _eq: syn::Token![=] = input.parse()?;
                let ty: Type = input.parse()?;
                Ok(Self(ty))
            }
        }
        let parsed: KeyArg = syn::parse2(list.tokens.clone())?;
        return Ok(Some(parsed.0));
    }
    Ok(None)
}

// ── Type helpers ────────────────────────────────────────────────────────────

/// If `ty` is `Arc<T>` (or `std::sync::Arc<T>` etc.), return `T`. Otherwise None.
fn extract_arc_inner(ty: &Type) -> Option<Type> {
    let Type::Path(tp) = ty else {
        return None;
    };
    let last = tp.path.segments.last()?;
    if last.ident != "Arc" {
        return None;
    }
    let PathArguments::AngleBracketed(args) = &last.arguments else {
        return None;
    };
    let mut iter = args.args.iter();
    let GenericArgument::Type(inner) = iter.next()? else {
        return None;
    };
    if iter.next().is_some() {
        // More than one type arg → not the Arc<T> we expect.
        return None;
    }
    Some(inner.clone())
}
