//! Typed platform fixture accessors over the frozen `wamn:postgres` capability.
//!
//! Runtime access uses the generated statement accessors of the fixture
//! package, and this crate authors no SQL. It covers the eleven operations that
//! `apps/platform_fixture/wamn.json` declares and nothing more.

mod error;
mod generated;
mod page;
mod scalar;
pub mod widget;
pub mod widget_maker;
pub mod widget_tag;

pub use error::{AccessError, AccessErrorKind};
pub use page::{Page, QueryInput};
