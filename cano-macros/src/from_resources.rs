//! Implementation of `#[derive(FromResources)]`.
//!
//! Generates a `Self::from_resources(res)` associated function that pulls every
//! field out of a `cano::Resources` map. Every field must be `Arc<T>`:
//!
//! - no attribute — the resource of type `T` (`Resources::get_arc::<T>()`);
//! - `#[res("name")]` — the resource named `name` (`Resources::get_named_arc::<T>("name")`).

use proc_macro2::TokenStream;
use quote::quote;
use syn::{
    Data, DataStruct, DeriveInput, Field, Fields, GenericArgument, LitStr, PathArguments, Type,
    parse2,
};

pub(crate) fn expand(input: TokenStream) -> syn::Result<TokenStream> {
    let derive_input: DeriveInput = parse2(input)?;
    let struct_name = &derive_input.ident;
    let (impl_generics, ty_generics, where_clause) = derive_input.generics.split_for_impl();

    let Data::Struct(DataStruct {
        fields: Fields::Named(named),
        ..
    }) = &derive_input.data
    else {
        return Err(syn::Error::new(
            struct_name.span(),
            "FromResources can only be derived for structs with named fields",
        ));
    };
    if named.named.is_empty() {
        return Err(syn::Error::new(
            struct_name.span(),
            "FromResources requires at least one field",
        ));
    }

    let mut assigns = Vec::with_capacity(named.named.len());
    for field in &named.named {
        let ident = field.ident.as_ref().expect("named field");
        let inner = extract_arc_inner(&field.ty).ok_or_else(|| {
            syn::Error::new_spanned(
                &field.ty,
                "FromResources: every field must be `Arc<T>` \
                 (it matches what `Resources::get_arc` returns)",
            )
        })?;
        assigns.push(match parse_name(field)? {
            Some(name) => quote!(#ident: __res.get_named_arc::<#inner>(#name)?),
            None => quote!(#ident: __res.get_arc::<#inner>()?),
        });
    }

    Ok(quote! {
        #[automatically_derived]
        impl #impl_generics #struct_name #ty_generics #where_clause {
            pub fn from_resources(__res: &::cano::Resources) -> ::cano::CanoResult<Self> {
                Ok(Self {
                    #(#assigns,)*
                })
            }
        }
    })
}

/// `#[res("name")]` on a field, if present.
fn parse_name(field: &Field) -> syn::Result<Option<LitStr>> {
    let mut found = None;
    for attr in &field.attrs {
        if !attr.path().is_ident("res") {
            continue;
        }
        if found.is_some() {
            return Err(syn::Error::new_spanned(
                attr,
                "duplicate #[res(...)] attribute on field",
            ));
        }
        let name: LitStr = attr.parse_args().map_err(|e| {
            syn::Error::new(
                e.span(),
                "#[res(...)] takes a string literal resource name: #[res(\"name\")]",
            )
        })?;
        found = Some(name);
    }
    Ok(found)
}

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
