//! Shared attribute-arg parser for the task macros.
//!
//! Recognised key: `state = T` — required when the macro is applied to an inherent
//! `impl X { ... }` block (it names the trait's `TState` parameter).

use proc_macro2::TokenStream;
use syn::Type;

#[derive(Default)]
pub(crate) struct AttrArgs {
    pub state: Option<Type>,
}

impl AttrArgs {
    pub fn parse(attr: TokenStream) -> syn::Result<Self> {
        let mut out = AttrArgs::default();
        if attr.is_empty() {
            return Ok(out);
        }
        let parser = syn::meta::parser(|meta| {
            if meta.path.is_ident("state") {
                if out.state.is_some() {
                    return Err(meta.error("duplicate `state` argument"));
                }
                out.state = Some(meta.value()?.parse()?);
                Ok(())
            } else {
                Err(meta.error("unsupported argument; expected `state = T`"))
            }
        });
        syn::parse::Parser::parse2(parser, attr)?;
        Ok(out)
    }
}

/// Fold a non-empty `Vec<syn::Error>` into a single `syn::Error` by chaining
/// all errors with [`syn::Error::combine`].
///
/// # Panics
///
/// Panics if `errors` is empty.
pub(crate) fn combine_errors(mut errors: Vec<syn::Error>) -> syn::Error {
    let mut iter = errors.drain(..);
    let mut acc = iter.next().expect("combine_errors called with empty vec");
    for e in iter {
        acc.combine(e);
    }
    acc
}
