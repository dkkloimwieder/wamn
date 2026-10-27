use std::path::{Path, PathBuf};

use anyhow::Result;
use tempfile::TempDir;

use super::{check, git};

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
    assert!(check(&root(&fixture), "main")?.is_empty());
    Ok(())
}

#[test]
fn declared_forward_bump_passes() -> Result<()> {
    let fixture = fixture()?;
    move_pin(&fixture, &fixture.new, "build(web/wfe): bump for the shell")?;
    assert!(check(&root(&fixture), "main")?.is_empty());
    Ok(())
}

#[test]
fn silent_change_fails_with_pointers_and_commit() -> Result<()> {
    let fixture = fixture()?;
    move_pin(&fixture, &fixture.new, "fix: digests follow main")?;
    let id = git(&root(&fixture), &["rev-parse", "HEAD"])?;
    let problems = check(&root(&fixture), "main")?;
    assert_eq!(problems.len(), 1);
    for expected in [
        "web/wfe",
        &fixture.old,
        &fixture.new,
        id.trim(),
        "fix: digests follow main",
        "no pointer-changing commit",
    ] {
        assert!(problems[0].contains(expected), "{}", problems[0]);
    }
    Ok(())
}

#[test]
fn declared_backward_move_fails() -> Result<()> {
    let fixture = fixture()?;
    advance_main(&fixture)?;
    git(&root(&fixture), &["merge", "--ff-only", "main"])?;
    move_pin(&fixture, &fixture.old, "build(web/wfe): bump old pin")?;
    let problems = check(&root(&fixture), "main")?;
    assert_eq!(problems.len(), 1);
    assert!(problems[0].contains("not a descendant of main"));
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
    assert!(check(&root(&fixture), "main")?.is_empty());
    Ok(())
}

#[test]
fn unrelated_commit_cannot_declare_a_bump() -> Result<()> {
    let fixture = fixture()?;
    move_pin(&fixture, &fixture.new, "fix: digests follow main")?;
    commit(&root(&fixture), "build(web/wfe): bump")?;
    assert_eq!(check(&root(&fixture), "main")?.len(), 1);
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
    assert!(check(&root(&fixture), "main")?.is_empty());
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
        assert_eq!(check(&root(&fixture), "main")?.len(), 1, "{subject}");
    }
    Ok(())
}

#[test]
fn unchanged_branch_cannot_keep_a_pin_behind_main() -> Result<()> {
    let fixture = fixture()?;
    advance_main(&fixture)?;
    let problems = check(&root(&fixture), "main")?;
    assert_eq!(problems.len(), 1);
    assert!(problems[0].contains("not a descendant of main"));
    assert!(problems[0].contains("<none>"));
    Ok(())
}
