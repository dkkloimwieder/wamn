//! R10(1) for TypeScript clients: Git holds no TypeScript client that the
//! build generates from contracts (docs/plan/platform-deploy.md R10(1)).
//!
//! A generated client lives in a `client-ts` directory. The host route
//! clients are written to `target/wamn/wamn_control` by
//! `web/host-route-clients.mjs`. The lint skips `apps/`, whose `generated/`
//! directories wamn-00rts.4 removes with its own lint.

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
        problems.require(!is_committed_client(path), || {
            format!("{path}: a generated TypeScript client is build output, not a tracked file")
        });
    }
}

fn is_committed_client(path: &str) -> bool {
    !path.starts_with("apps/") && path.split('/').any(|component| component == "client-ts")
}

#[cfg(test)]
mod tests {
    use super::is_committed_client;

    #[test]
    fn a_client_ts_file_outside_apps_is_refused() {
        assert!(is_committed_client(
            "crates/catalog/model/src/host_route/generated/client-ts/index.ts"
        ));
        assert!(!is_committed_client(
            "apps/wamn_wms/generated/client-ts/index.ts"
        ));
        assert!(!is_committed_client(
            "crates/schema/generator/src/client_ts.rs"
        ));
    }
}
