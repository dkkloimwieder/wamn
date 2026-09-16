//! Formatting of the Rust source the generator emits.
//!
//! Emitters write source by hand, so their line breaks and indentation are
//! whatever the emitting code happened to spell. Running `rustfmt` over the
//! bytes before they become a [`crate::GeneratedFile`] makes the committed
//! artifacts equal what `cargo fmt` produces, and makes writing and checking
//! agree: the drift check compares the same formatted bytes the write would
//! have placed on disk.

use std::io::Write as _;
use std::process::{Command, Stdio};

/// The style the emitted Rust is formatted in. Every workspace here is edition
/// 2024, which selects the 2024 style edition, so `cargo fmt` over the tree
/// produces these same bytes.
///
/// The repository has no `rustfmt.toml`, so the crate edition alone sets the
/// style and this one flag is the whole configuration. Anyone who adds a
/// `rustfmt.toml` must make this flag agree with it: a `style_edition` there is
/// not guaranteed to format the same as the edition default, and a difference
/// would split what the generator writes from what `cargo fmt` produces.
const EDITION: &str = "2024";

/// Format one emitted Rust source file.
///
/// # Panics
///
/// Panics when `rustfmt` cannot be run, or refuses the source. `rustfmt` is a
/// declared `rust-toolchain.toml` component, so it is always installed, and
/// source it cannot parse is a defect in the emitter that produced it rather
/// than anything a caller supplied.
pub(crate) fn format_rust(source: &[u8]) -> Vec<u8> {
    let mut rustfmt = Command::new("rustfmt")
        .args(["--edition", EDITION, "--emit", "stdout", "--quiet"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("rustfmt is a declared toolchain component");
    rustfmt
        .stdin
        .take()
        .expect("rustfmt stdin was piped")
        .write_all(source)
        .expect("rustfmt reads the whole source before emitting");
    let formatted = rustfmt
        .wait_with_output()
        .expect("rustfmt runs to completion");
    assert!(
        formatted.status.success(),
        "rustfmt refused emitted Rust source: {}",
        String::from_utf8_lossy(&formatted.stderr)
    );
    formatted.stdout
}
