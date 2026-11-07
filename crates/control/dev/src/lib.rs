//! Development operations and process ownership for the CLI and test support.

pub mod dev;
pub mod git_source;
pub mod owned_command;
pub mod source_deploy;

#[cfg(test)]
mod pat_secrets_tests;
