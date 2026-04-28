//! Shared attribute-arg parser for `#[node(...)]` / `#[task(...)]`.
//!
//! Recognised keys:
//! - `state = T` — required when the macro is applied to an inherent `impl X { ... }`
//!   block (we need to know the trait's `TState` parameter).
//! - `key = K` — optional resource-key type. Defaults to `::std::borrow::Cow<'static, str>`
//!   when omitted (matches the trait default).

use proc_macro2::TokenStream;
use syn::Type;

#[derive(Default)]
pub(crate) struct AttrArgs {
    pub state: Option<Type>,
    pub key: Option<Type>,
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
            } else {
                Err(meta.error("unsupported argument; expected `state = T` or `key = K`"))
            }
        });
        syn::parse::Parser::parse2(parser, attr)?;
        Ok(out)
    }
}
