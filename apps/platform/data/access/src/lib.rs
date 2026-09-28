//! The cursor, page and scalar code that generated data functions call.
//!
//! A generated data function parses each request value in its one wire
//! spelling, reads one keyset page, and mints the cursor that continues it.
//! Each package once copied this code into its data crate. It lives here once,
//! and each generated data function translates [`Invalid`] into its own
//! package's refusal.

pub mod cursor;
pub mod page;
pub mod scalar;

pub use cursor::Direction;
pub use page::Page;

/// The bounds of one range filter, each inclusive and optional.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Range {
    pub min: Option<String>,
    pub max: Option<String>,
}

/// A value that is not in its one wire spelling. The caller names the field.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Invalid;
