//! Procedural macros for compile-time `nghttp2` descriptors.
//!
//! `nghttp2` represents a header name and value as a length-delimited byte slice
//! (`nghttp2_nv`). Building those slices from string literals at runtime costs an
//! allocation and a copy per header. The macros in this crate embed the bytes in
//! the binary instead, so the safe wrapper can hand the C library pointers into
//! static data.

use proc_macro::TokenStream;
use quote::quote;
use syn::{LitStr, Token, parse_macro_input};

/// Expands `nv!("name", "value")` to a pair of static byte strings.
///
/// Both arguments must be string literals. The expansion has no runtime cost: the
/// bytes live in the binary and the returned references are `'static`. Callers
/// turn the pair into a header descriptor with the copy flags cleared.
#[proc_macro]
pub fn nv(input: TokenStream) -> TokenStream {
    let NvInput { name, value, .. } = parse_macro_input!(input as NvInput);
    quote! {
        ((#name).as_bytes(), (#value).as_bytes())
    }
    .into()
}

struct NvInput {
    name: LitStr,
    _comma: Token![,],
    value: LitStr,
}

impl syn::parse::Parse for NvInput {
    fn parse(input: syn::parse::ParseStream<'_>) -> syn::Result<Self> {
        Ok(Self {
            name: input.parse()?,
            _comma: input.parse()?,
            value: input.parse()?,
        })
    }
}
