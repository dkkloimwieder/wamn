//! Every package under `apps/` authors its manifest in `wamn.k`.
//!
//! The generator compiles `wamn.k` to `generated/wamn.json`, so a `wamn.json`
//! at a package root is refused (docs/plan/manifest-authoring.md §4.3, ruling
//! 8). Test fixtures outside `apps/` stay JSON, so the lint reads only `apps/`.

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

#[cfg(test)]
mod tests {
    use super::{Problems, check};

    /// A package with `wamn.k` passes, and a root `wamn.json` fails with its
    /// package named, whether or not `wamn.k` is beside it.
    #[test]
    fn a_root_wamn_json_under_apps_is_refused() {
        let root =
            std::env::temp_dir().join(format!("wamn-authored-manifests-{}", std::process::id()));
        let authored = root.join("apps/authored");
        std::fs::create_dir_all(authored.join("generated")).expect("create the fixture tree");
        std::fs::write(authored.join("wamn.k"), "").expect("write wamn.k");
        std::fs::write(authored.join("generated/wamn.json"), "{}").expect("write the compile");

        let mut clean = Problems::default();
        check(&root, &mut clean);

        let hand_written = root.join("apps/hand_written");
        std::fs::create_dir_all(&hand_written).expect("create the hand-written package");
        std::fs::write(hand_written.join("wamn.json"), "{}").expect("write wamn.json");
        std::fs::write(authored.join("wamn.json"), "{}").expect("write a second manifest");
        let mut offending = Problems::default();
        check(&root, &mut offending);
        std::fs::remove_dir_all(&root).expect("remove the fixture tree");

        assert!(
            clean.0.is_empty(),
            "a converted tree must pass: {:?}",
            clean.0
        );
        assert_eq!(
            offending.0,
            [
                "apps/authored: the manifest is authored in wamn.k; wamn.json is generated",
                "apps/hand_written: the manifest is authored in wamn.k; wamn.json is generated",
            ]
        );
    }
}
