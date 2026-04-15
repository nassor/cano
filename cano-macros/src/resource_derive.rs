//! Implementation of `#[derive(Resource)]`.
//!
//! Generates an empty `cano::Resource` impl that uses the trait's default
//! no-op `setup` / `teardown`.
//!
//! The `Resource` trait on this branch is already rewritten by the in-house
//! async-trait rewriter (`#[cano::resource]`), so generated impls inherit the
//! correct method shapes from the trait defaults and need no `#[async_trait]`.

use proc_macro2::TokenStream;
use quote::quote;
use syn::{DeriveInput, parse2};

pub(crate) fn expand(input: TokenStream) -> syn::Result<TokenStream> {
    let derive_input: DeriveInput = parse2(input)?;
    let struct_name = &derive_input.ident;
    let (impl_generics, ty_generics, where_clause) = derive_input.generics.split_for_impl();

    Ok(quote! {
        #[automatically_derived]
        impl #impl_generics ::cano::Resource for #struct_name #ty_generics #where_clause {}
    })
}
