//! Management-owned authoring persistence.

use sha2::{Digest as _, Sha256};

pub mod drafts;
pub mod test_orchestration;
pub mod test_sets;

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
