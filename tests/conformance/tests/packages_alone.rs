//! `tools/check-packages-alone` refuses a package that compiles only through a
//! dependency feature another package of the workspace turns on (wamn-bcplj).

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const TOOL: &str = "tools/check-packages-alone";

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("wamn packages alone {}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir(&path).expect("create packages-alone test directory");
        Self(path)
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("conformance package must live at tests/conformance")
        .to_path_buf()
}

fn write(root: &Path, name: &str, contents: &str) {
    let path = root.join(name);
    fs::create_dir_all(path.parent().expect("fixture file has a parent"))
        .expect("create fixture directory");
    fs::write(path, contents).expect("write fixture file");
}

fn package(name: &str, dependency: &str) -> String {
    format!(
        "[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n\
         [lib]\npath = \"lib.rs\"\n\n[dependencies]\n{dependency}"
    )
}

/// A workspace where `user` calls `dep::extra`, which only `enabler` turns on.
fn fixture(root: &Path) {
    write(
        root,
        "Cargo.toml",
        "[workspace]\nresolver = \"2\"\nmembers = [\"dep\", \"enabler\", \"user\"]\n",
    );
    write(
        root,
        "dep/Cargo.toml",
        "[package]\nname = \"dep\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n\
         [lib]\npath = \"lib.rs\"\n\n[features]\nextra = []\n",
    );
    write(
        root,
        "dep/lib.rs",
        "#[cfg(feature = \"extra\")]\npub fn extra() {}\n",
    );
    write(
        root,
        "enabler/Cargo.toml",
        &package(
            "enabler",
            "dep = { path = \"../dep\", features = [\"extra\"] }\n",
        ),
    );
    write(root, "enabler/lib.rs", "");
    write(
        root,
        "user/Cargo.toml",
        &package("user", "dep = { path = \"../dep\" }\n"),
    );
    write(
        root,
        "user/lib.rs",
        "pub fn call() {\n    dep::extra();\n}\n",
    );
}

fn cargo(root: &Path, arguments: &[&str]) -> Output {
    Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned()))
        .args(arguments)
        .arg("--manifest-path")
        .arg(root.join("Cargo.toml"))
        .env("CARGO_TARGET_DIR", root.join("target"))
        .output()
        .expect("run cargo on the fixture")
}

fn run_tool(root: &Path) -> Output {
    Command::new(repository_root().join(TOOL))
        .arg(root.join("Cargo.toml"))
        .env("CARGO_TARGET_DIR", root.join("target"))
        .output()
        .expect("run check-packages-alone")
}

#[test]
fn a_package_that_needs_a_unified_feature_fails_on_its_own() {
    let directory = TestDirectory::new();
    let root = &directory.0;
    fixture(root);
    assert!(
        cargo(root, &["generate-lockfile", "--offline"])
            .status
            .success()
    );
    assert!(
        cargo(root, &["check", "--workspace", "--quiet"])
            .status
            .success(),
        "the workspace build must pass, or the fixture does not show feature unification"
    );

    let refused = run_tool(root);
    let stdout = String::from_utf8_lossy(&refused.stdout);
    let stderr = String::from_utf8_lossy(&refused.stderr);
    assert_eq!(
        refused.status.code(),
        Some(1),
        "stdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(stdout.contains("check -p dep: ok"), "{stdout}");
    assert!(stdout.contains("check -p enabler: ok"), "{stdout}");
    assert!(stdout.contains("check -p user: failed"), "{stdout}");
    assert!(
        stderr.contains("these packages do not compile on their own: user\n"),
        "{stderr}"
    );

    write(
        root,
        "user/Cargo.toml",
        &package(
            "user",
            "dep = { path = \"../dep\", features = [\"extra\"] }\n",
        ),
    );
    let accepted = run_tool(root);
    assert!(
        accepted.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&accepted.stdout),
        String::from_utf8_lossy(&accepted.stderr)
    );
}
