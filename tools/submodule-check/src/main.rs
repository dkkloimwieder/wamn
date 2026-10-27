//! Refuse undeclared or backward submodule pins before train integration.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::{Command, ExitCode, Output};

use anyhow::{Context, Result, bail, ensure};

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("submodule-check: {error:#}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let main = args.next().unwrap_or_else(|| "main".into());
    ensure!(
        args.next().is_none(),
        "usage: wamn-submodule-check [main-ref]"
    );
    let root = git(Path::new("."), &["rev-parse", "--show-toplevel"])?;
    let problems = check(Path::new(root.trim()), &main)?;
    ensure!(problems.is_empty(), "{}", problems.join("\n"));
    println!("submodule-check: PASS against {main}");
    Ok(())
}

fn check(root: &Path, main: &str) -> Result<Vec<String>> {
    let main = git(
        root,
        &["rev-parse", "--verify", &format!("{main}^{{commit}}")],
    )?;
    let main = main.trim();
    let base = git(root, &["merge-base", "HEAD", main])?;
    let base = base.trim();
    let baseline = pins(root, base)?;
    let upstream = pins(root, main)?;
    let branch = pins(root, "HEAD")?;
    let mut paths: Vec<_> = baseline
        .keys()
        .chain(upstream.keys())
        .chain(branch.keys())
        .collect();
    paths.sort();
    paths.dedup();
    let mut problems = Vec::new();
    for path in paths {
        let before = baseline.get(path);
        let current = branch.get(path);
        let on_main = upstream.get(path);
        if current == before && current == on_main {
            continue;
        }
        let changes = changes(root, base, path)?;
        // Past pins read "pin wfe 9248538 for ...", so the last path segment
        // and the word pin count too.
        let declared = |subject: &str| {
            names_path(subject, path) && (has_word(subject, "bump") || has_word(subject, "pin"))
        };
        let mut reasons = Vec::new();
        if current != before && !changes.values().any(|subject| declared(subject)) {
            reasons.push(
                "no pointer-changing commit names the submodule and the word bump or pin"
                    .to_owned(),
            );
        }
        let reverted = changes
            .values()
            .any(|subject| declared(subject) && has_word(subject, "revert"));
        if current != on_main && !reverted {
            match (on_main, current) {
                (Some(old), Some(new)) => {
                    if let Err(error) = require_descendant(&root.join(path), old, new) {
                        reasons.push(format!("{error:#}"));
                    }
                }
                _ => reasons
                    .push("cannot compare ancestry with a missing main or branch pointer".into()),
            }
        }
        if !reasons.is_empty() {
            let commits = changes
                .iter()
                .map(|(id, subject)| format!("  {id} {subject}"))
                .collect::<Vec<_>>()
                .join("\n");
            problems.push(format!(
                "{path}: {}\n  merge-base {base}: {}\n  main {main}: {}\n  branch HEAD: {}\n  pointer-changing commits since merge-base:\n{}",
                reasons.join("; "), before.map_or("<absent>", String::as_str), on_main.map_or("<absent>", String::as_str), current.map_or("<absent>", String::as_str),
                if commits.is_empty() { "  <none>" } else { &commits }
            ));
        }
    }
    Ok(problems)
}

fn pins(root: &Path, revision: &str) -> Result<BTreeMap<String, String>> {
    let tree = git(root, &["ls-tree", "-rz", "--full-tree", revision])?;
    Ok(tree
        .split('\0')
        .filter_map(|entry| {
            let (metadata, path) = entry.split_once('\t')?;
            let mut fields = metadata.split_whitespace();
            if fields.next()? != "160000" {
                return None;
            }
            fields.next()?;
            Some((path.to_owned(), fields.next()?.to_owned()))
        })
        .collect())
}

fn changes(root: &Path, base: &str, path: &str) -> Result<BTreeMap<String, String>> {
    let log = git(
        root,
        &[
            "log",
            "--full-history",
            "-m",
            "--format=%H%x09%s",
            &format!("{base}..HEAD"),
            "--",
            path,
        ],
    )?;
    let mut changes = BTreeMap::new();
    for line in log.lines() {
        let (id, subject) = line.split_once('\t').context("invalid git log record")?;
        // Full history can include merges whose pointer did not change.
        if !git(
            root,
            &[
                "diff-tree",
                "--root",
                "-m",
                "-r",
                "--no-commit-id",
                "--name-only",
                id,
                "--",
                path,
            ],
        )?
        .is_empty()
        {
            changes.insert(id.to_owned(), subject.to_owned());
        }
    }
    Ok(changes)
}

fn names_path(subject: &str, path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    subject
        .split(|c: char| !c.is_alphanumeric() && !matches!(c, '/' | '_' | '.' | '-'))
        .any(|word| word == path || word == name)
}

fn has_word(subject: &str, word: &str) -> bool {
    subject
        .split(|c: char| !c.is_alphanumeric())
        .any(|part| part.eq_ignore_ascii_case(word))
}

fn require_descendant(module: &Path, main: &str, branch: &str) -> Result<()> {
    ensure!(
        git(module, &["rev-parse", "--show-prefix"])?
            .trim()
            .is_empty(),
        "initialize the submodule and fetch its history"
    );
    let output = git_output(module, &["merge-base", "--is-ancestor", main, branch])?;
    match output.status.code() {
        Some(0) => Ok(()),
        Some(1) => bail!(
            "branch pointer is not a descendant of main (a declared bump must also say revert)"
        ),
        _ => bail!(
            "submodule ancestry is unavailable: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ),
    }
}

fn git_output(root: &Path, args: &[&str]) -> Result<Output> {
    Command::new("git")
        .current_dir(root)
        .args(args)
        .output()
        .with_context(|| format!("run git {args:?} in {}", root.display()))
}

fn git(root: &Path, args: &[&str]) -> Result<String> {
    let output = git_output(root, args)?;
    ensure!(
        output.status.success(),
        "git {args:?} in {} failed: {}",
        root.display(),
        String::from_utf8_lossy(&output.stderr).trim()
    );
    String::from_utf8(output.stdout).context("git output is not UTF-8")
}

#[cfg(test)]
mod tests;
