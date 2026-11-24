//! Project authorization reads and their shared cache.

mod project_reads;
mod store;

pub use store::Authorization;

#[cfg(test)]
mod tests;
