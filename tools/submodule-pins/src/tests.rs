use std::path::{Path, PathBuf};

use anyhow::Result;
use tempfile::TempDir;

use super::{diagnostics, git};

struct Fixture {
    directory: TempDir,
    old: String,
    new: String,
}

fn root(fixture: &Fixture) -> PathBuf {
    fixture.directory.path().join("repo")
}

fn init(path: &Path) -> Result<()> {
    std::fs::create_dir(path)?;
    git(path, &["init", "-b", "main"])?;
    git(path, &["config", "user.name", "Pin test"])?;
    git(path, &["config", "user.email", "pin@example.invalid"])?;
    git(path, &["config", "commit.gpgsign", "false"])?;
    git(path, &["config", "core.hooksPath", "/dev/null"])?;
    Ok(())
}

fn commit(path: &Path, subject: &str) -> Result<()> {
    git(path, &["commit", "--allow-empty", "-m", subject])?;
    Ok(())
}

fn fixture() -> Result<Fixture> {
    let directory = tempfile::tempdir()?;
    let source = directory.path().join("source");
    init(&source)?;
    commit(&source, "initial module")?;
    let old = git(&source, &["rev-parse", "HEAD"])?.trim().to_owned();
    commit(&source, "forward module")?;
    let new = git(&source, &["rev-parse", "HEAD"])?.trim().to_owned();
    let repo = directory.path().join("repo");
    init(&repo)?;
    git(
        &repo,
        &[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            source.to_str().unwrap(),
            "web/wfe",
        ],
    )?;
    git(&repo.join("web/wfe"), &["checkout", &old])?;
    git(&repo, &["add", ".gitmodules", "web/wfe"])?;
    commit(&repo, "initial parent")?;
    git(&repo, &["checkout", "-b", "topic"])?;
    Ok(Fixture {
        directory,
        old,
        new,
    })
}

fn move_pin(fixture: &Fixture, pointer: &str, subject: &str) -> Result<()> {
    let repo = root(fixture);
    git(&repo.join("web/wfe"), &["checkout", pointer])?;
    git(&repo, &["add", "web/wfe"])?;
    commit(&repo, subject)
}

fn advance_main(fixture: &Fixture) -> Result<()> {
    git(&root(fixture), &["checkout", "main"])?;
    move_pin(fixture, &fixture.new, "build(web/wfe): bump forward")?;
    git(&root(fixture), &["checkout", "topic"])?;
    Ok(())
}

#[test]
fn unchanged_pointer_passes() -> Result<()> {
    let fixture = fixture()?;
    assert!(diagnostics(&root(&fixture), "main")?.is_empty());
    Ok(())
}

#[test]
fn declared_forward_bump_passes() -> Result<()> {
    let fixture = fixture()?;
    move_pin(&fixture, &fixture.new, "build(web/wfe): bump for the shell")?;
    assert!(diagnostics(&root(&fixture), "main")?.is_empty());
    Ok(())
}

#[test]
fn silent_change_is_refused() -> Result<()> {
    let fixture = fixture()?;
    move_pin(&fixture, &fixture.new, "fix: digests follow main")?;
    let problems = diagnostics(&root(&fixture), "main")?;
    assert!(!problems.is_empty());
    Ok(())
}

#[test]
fn declared_backward_move_fails() -> Result<()> {
    let fixture = fixture()?;
    advance_main(&fixture)?;
    git(&root(&fixture), &["merge", "--ff-only", "main"])?;
    move_pin(&fixture, &fixture.old, "build(web/wfe): bump old pin")?;
    let problems = diagnostics(&root(&fixture), "main")?;
    assert!(!problems.is_empty());
    Ok(())
}

#[test]
fn declared_revert_passes() -> Result<()> {
    let fixture = fixture()?;
    advance_main(&fixture)?;
    git(&root(&fixture), &["merge", "--ff-only", "main"])?;
    move_pin(
        &fixture,
        &fixture.old,
        "build(web/wfe): bump to revert the broken pin",
    )?;
    assert!(diagnostics(&root(&fixture), "main")?.is_empty());
    Ok(())
}

#[test]
fn unrelated_commit_cannot_declare_a_bump() -> Result<()> {
    let fixture = fixture()?;
    move_pin(&fixture, &fixture.new, "fix: digests follow main")?;
    commit(&root(&fixture), "build(web/wfe): bump")?;
    assert!(!diagnostics(&root(&fixture), "main")?.is_empty());
    Ok(())
}

#[test]
fn the_historical_pin_subject_declares_a_bump() -> Result<()> {
    // 3f7fac62e on main.
    let fixture = fixture()?;
    move_pin(
        &fixture,
        &fixture.new,
        "build(wamn-rbns9.6): pin wfe 9248538 for shell() and the send-confirmed fix",
    )?;
    assert!(diagnostics(&root(&fixture), "main")?.is_empty());
    Ok(())
}

#[test]
fn declaration_requires_the_submodule_and_word() -> Result<()> {
    for subject in [
        "build(web): bump pin",
        "build(web/wfe): bumped pinned",
        "build(web/wfe-extra): bump pin",
    ] {
        let fixture = fixture()?;
        move_pin(&fixture, &fixture.new, subject)?;
        assert!(
            !diagnostics(&root(&fixture), "main")?.is_empty(),
            "{subject}"
        );
    }
    Ok(())
}

#[test]
fn unchanged_branch_cannot_keep_a_pin_behind_main() -> Result<()> {
    let fixture = fixture()?;
    advance_main(&fixture)?;
    let problems = diagnostics(&root(&fixture), "main")?;
    assert!(!problems.is_empty());
    Ok(())
}
