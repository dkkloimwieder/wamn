//! Management-owned authoring persistence.
//!
//! The two stores here live in DIFFERENT DATABASES and nothing joins them:
//! [`test_orchestration`] is the control-database half, [`admission`] is the
//! project-database half. A fact one needs from the other crosses as an
//! already-observed value.
//!
//! There is no draft store (wamn-0h0g.8.5.5). A draft is a CLIENT-SIDE file — a
//! studio buffer, a git working tree — and the wiring document's own content
//! hash is its identity, so nothing here persists a mutable authored document.

use sha2::{Digest as _, Sha256};

pub mod admission;
pub mod test_orchestration;

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
