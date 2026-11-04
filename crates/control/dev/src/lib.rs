//! Development operations and process ownership for the CLI and test support.

pub mod dev;
pub mod git_source;
pub mod owned_command;

#[cfg(test)]
mod pat_secrets_tests;
