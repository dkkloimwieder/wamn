//! Management-owned authoring persistence.
//!
//! The two stores here live in DIFFERENT DATABASES and nothing joins them:
//! [`drafts`] and [`test_orchestration`] are the control-database half,
//! [`admission`] is the project-database half. A fact one needs from the other
//! crosses as an already-observed value.

use sha2::{Digest as _, Sha256};

pub mod admission;
pub mod drafts;
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
