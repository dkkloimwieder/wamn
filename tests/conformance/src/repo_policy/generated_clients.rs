//! R10(1): Git holds nothing that the build regenerates byte-for-byte from
//! sources (docs/plan/platform-deploy.md R10(1)).
//!
//! The lint refuses a tracked file that is:
//! - in a `client-ts` directory: a TypeScript client generated from contracts.
//!   The host route clients are written to `target/wamn/wamn_control` by
//!   `web/host-route-clients.mjs`.
//! - in a `generated` or `.sqlx` directory under `apps/`: a package's build
//!   output, which `wamn build` writes to `apps/target/wamn/<package>`.
//! - a `package-identity.json` outside a `tests` directory.
//!
//! Hand-written fixtures and golden expectation files under `tests/` stay
//! tracked: they are inputs and expectations, not build output.

use std::path::Path;
use std::process::Command;

use super::Problems;

pub(super) fn check(root: &Path, problems: &mut Problems) {
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
    let under_tests = components.contains(&"tests");
    components.contains(&"client-ts")
        || (components.first() == Some(&"apps")
            && components
                .iter()
                .any(|component| *component == "generated" || *component == ".sqlx"))
        || (components.last() == Some(&"package-identity.json") && !under_tests)
}

#[cfg(test)]
mod tests {
    use super::is_build_output;

    #[test]
    fn build_output_is_refused_and_test_fixtures_stay() {
        for refused in [
            "crates/catalog/model/src/host_route/generated/client-ts/index.ts",
            "apps/wamn_wms/generated/client-ts/index.ts",
            "apps/wamn_wms/generated/sql/item/get.sql",
            "apps/wamn_wms/tests/.sqlx/query-0.json",
            "apps/wamn_wms/generated/package-identity.json",
            "crates/catalog/model/package-identity.json",
        ] {
            assert!(is_build_output(refused), "{refused}");
        }
        for kept in [
            "crates/schema/generator/src/client_ts.rs",
            "apps/wamn_wms/wamn.k",
            "crates/control/lib/tests/fixtures/component_package/generated/package-identity.json",
            "crates/control/lib/tests/support/dev_package/package-identity.json",
        ] {
            assert!(!is_build_output(kept), "{kept}");
        }
    }
}
