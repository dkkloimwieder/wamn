//! Every package under `apps/` authors its manifest in `wamn.k`.
//!
//! `wamn build` compiles `wamn.k` to `wamn.json` in the package's build output,
//! so a `wamn.json` at a package root is refused
//! (docs/architecture/overview.md#application-ownership). Test fixtures outside
//! `apps/` stay JSON, so the lint reads only `apps/`.

use std::path::Path;

use super::Problems;

pub(super) fn check(root: &Path, problems: &mut Problems) {
    let entries = match std::fs::read_dir(root.join("apps")) {
        Ok(entries) => entries,
        Err(error) => {
            problems.push(format!("apps: {error}"));
            return;
        }
    };
    let mut packages = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.join("wamn.json").is_file())
        .collect::<Vec<_>>();
    packages.sort();
    for package in packages {
        let name = package.file_name().unwrap_or_default().to_string_lossy();
        problems.push(format!(
            "apps/{name}: the manifest is authored in wamn.k; wamn.json is generated"
        ));
    }
}
