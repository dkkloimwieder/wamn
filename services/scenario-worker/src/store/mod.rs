//! Management-owned authoring reads.
//!
//! One store remains: [`admission`], the project-database half, which is where
//! the gate verb now lives. Its control-database counterpart went with the
//! composition machinery (wamn-0h0g.8.5.5) — a store whose only writer and only
//! reader were both deleted by the same change is not a store.
//!
//! There is no draft store either. A draft is a CLIENT-SIDE file — a studio
//! buffer, a git working tree — and the wiring document's own content hash is
//! its identity, so nothing here persists a mutable authored document.

use sha2::{Digest as _, Sha256};

pub mod admission;

pub(crate) fn sha256(input: &[u8]) -> String {
    let digest = Sha256::digest(input);
    let mut output = String::with_capacity(71);
    output.push_str("sha256:");
    for byte in digest {
        use std::fmt::Write as _;
        write!(output, "{byte:02x}").expect("writing to String cannot fail");
    }
    output
}
