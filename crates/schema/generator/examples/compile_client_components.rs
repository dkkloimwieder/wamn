//! compiles emitted descriptions against the pinned wfe generator package.
//!
//! Run `pnpm install --offline` at the repository root first, then:
//!
//! ```bash
//! cargo run --offline -p wamn-compiler --example compile_client_components
//! ```
//!
//! The existing manual TypeScript test writes only to a temporary directory.
//! The integration test uses this same test. Application bindings are validated
//! by their application, independently of these presentation descriptions.

#[path = "../tests/support/platform_fixture.rs"]
mod fixture;
#[path = "../tests/support/client_component_compile.rs"]
mod typescript;

fn main() -> anyhow::Result<()> {
    typescript::compile_descriptions(&fixture::client_release())
}
