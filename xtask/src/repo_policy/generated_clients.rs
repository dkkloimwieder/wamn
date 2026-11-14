//! R10(1): Git holds nothing that the build regenerates byte-for-byte from
//! sources (docs/architecture/overview.md#application-ownership).
//!
//! The lint refuses a tracked file that is:
//! - in a `client-ts` directory: a TypeScript client generated from contracts.
//!   The host route clients are written to `apps/target/wamn/wamn_control` by
//!   `cargo xtask host-route-clients`.
//! - in a `generated` or `.sqlx` directory under `apps/`: a package's build
//!   output, which `wamn build` writes to `apps/target/wamn/<package>`.
//!
//! Hand-written fixtures and golden expectation files under `tests/` stay
//! tracked: they are inputs and expectations, not build output.

use std::path::Path;
use std::process::Command;

use super::Problems;

pub(super) fn report_problems(root: &Path, problems: &mut Problems) {
    let output = match Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["ls-files", "-z"])
        .output()
    {
        Ok(output) if output.status.success() => output,
        Ok(output) => {
            problems.push(format!(
                "git ls-files failed:\n{}",
                String::from_utf8_lossy(&output.stderr)
            ));
            return;
        }
        Err(error) => {
            problems.push(format!("list tracked files: {error}"));
            return;
        }
    };
    for path in output
        .stdout
        .split(|byte| *byte == 0)
        .filter_map(|path| std::str::from_utf8(path).ok())
    {
        problems.require(!is_build_output(path), || {
            format!("{path}: generated output is build output, not a tracked file")
        });
    }
}

fn is_build_output(path: &str) -> bool {
    let components = path.split('/').collect::<Vec<_>>();
    components.contains(&"client-ts")
        || (components.first() == Some(&"apps")
            && components
                .iter()
                .any(|component| *component == "generated" || *component == ".sqlx"))
}
