//! Type-checks emitted descriptions against the pinned wfe generator package.
//!
//! Run `pnpm install --offline` at the repository root first, then:
//!
//! ```bash
//! cargo run --offline -p wamn-schema-generator --example check_client_components
//! ```
//!
//! The existing manual TypeScript check writes only to a temporary directory.
//! The integration test uses this same check. Application bindings are checked
//! by their application, independently of these presentation descriptions.

#[path = "../tests/support/platform_fixture.rs"]
mod fixture;
#[path = "../tests/support/client_component_check.rs"]
mod typescript;

fn main() -> anyhow::Result<()> {
    typescript::check_descriptions(&fixture::client_release())
}
