//! Shared attribute-arg parser for `#[node(...)]` / `#[task(...)]` /
//! `#[compensatable_task(...)]`.
//!
//! Recognised keys:
//! - `state = T` — required when the macro is applied to an inherent `impl X { ... }`
//!   block (we need to know the trait's `TState` parameter).
//! - `key = K` — optional resource-key type. Defaults to `::std::borrow::Cow<'static, str>`
//!   when omitted (matches the trait default).
//! - `compensatable` — bare flag accepted by `#[task(...)]` to route an inherent impl to
//!   the `CompensatableTask` codegen (equivalent to `#[compensatable_task(...)]`).

use proc_macro2::TokenStream;
use syn::Type;

#[derive(Default)]
pub(crate) struct AttrArgs {
    pub state: Option<Type>,
    pub key: Option<Type>,
    pub compensatable: bool,
}

impl AttrArgs {
    pub fn parse(attr: TokenStream) -> syn::Result<Self> {
        let mut out = AttrArgs::default();
        if attr.is_empty() {
            return Ok(out);
        }
        let parser = syn::meta::parser(|meta| {
            if meta.path.is_ident("state") {
                out.state = Some(meta.value()?.parse()?);
                Ok(())
            } else if meta.path.is_ident("key") {
                out.key = Some(meta.value()?.parse()?);
                Ok(())
            } else if meta.path.is_ident("compensatable") {
                out.compensatable = true;
                Ok(())
            } else {
                Err(meta.error(
                    "unsupported argument; expected `state = T`, `key = K`, or `compensatable`",
                ))
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
