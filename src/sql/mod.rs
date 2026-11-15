//! Engine-agnostic SQL helpers: identifier/literal quoting and a lexer.

pub mod ident;
pub mod lexer;

pub use ident::{quote_ident, quote_literal, quote_qualified};
