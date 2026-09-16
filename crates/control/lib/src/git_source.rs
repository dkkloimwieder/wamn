//! Whole-worktree Git source state for the callers that qualify a revision.
//!
//! One reader answers both questions the delivery and development owners ask of
//! a checkout: which commit is at `HEAD`, and does the worktree hold anything
//! the commit does not. `git status --porcelain=v2` answers both from the same
//! process, so the two cannot disagree about which revision was observed.

use std::error::Error;
use std::ffi::OsString;
use std::fmt;
use std::io;
use std::os::unix::ffi::OsStringExt as _;
use std::path::{Path, PathBuf};
use std::process::Output;

use tokio::process::Command;

/// Stable category of a Git source-state failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GitSourceErrorKind {
    Discover,
    Inspect,
}

/// Failure to discover or inspect one originating Git repository.
#[derive(Debug)]
pub struct GitSourceError {
    kind: GitSourceErrorKind,
    operation: &'static str,
    repository: PathBuf,
    detail: Box<str>,
    source: Option<io::Error>,
}

impl GitSourceError {
    /// A Git process that could not be executed at all.
    pub fn io(
        kind: GitSourceErrorKind,
        operation: &'static str,
        repository: &Path,
        source: io::Error,
    ) -> Self {
        Self {
            kind,
            operation,
            repository: repository.to_owned(),
            detail: "Git process could not be executed".into(),
            source: Some(source),
        }
    }

    /// A Git process that ran and refused, rendered from its own diagnostics.
    pub fn command(
        kind: GitSourceErrorKind,
        operation: &'static str,
        repository: &Path,
        output: &Output,
    ) -> Self {
        let detail = String::from_utf8_lossy(trim_ascii(&output.stderr));
        let detail = if detail.is_empty() {
            format!("git exited with {}", output.status)
        } else {
            detail.into_owned()
        };
        Self {
            kind,
            operation,
            repository: repository.to_owned(),
            detail: detail.into_boxed_str(),
            source: None,
        }
    }

    /// A Git process that succeeded and returned output this reader cannot use.
    pub fn output(
        kind: GitSourceErrorKind,
        operation: &'static str,
        repository: &Path,
        detail: impl Into<Box<str>>,
    ) -> Self {
        Self {
            kind,
            operation,
            repository: repository.to_owned(),
            detail: detail.into(),
            source: None,
        }
    }

    /// Stable error category.
    pub const fn kind(&self) -> GitSourceErrorKind {
        self.kind
    }
}

impl fmt::Display for GitSourceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "cannot {} Git repository {}: {}",
            self.operation,
            self.repository.display(),
            self.detail
        )?;
        if let Some(source) = &self.source {
            write!(formatter, ": {source}")?;
        }
        Ok(())
    }
}

impl Error for GitSourceError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        self.source.as_ref().map(|source| source as _)
    }
}

/// Whole-worktree source state, including untracked non-ignored files.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GitSourceState {
    Clean,
    Dirty,
}

/// One repository-grained source observation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GitSourceSnapshot {
    repository_root: PathBuf,
    source_commit: Box<str>,
    state: GitSourceState,
}

impl GitSourceSnapshot {
    /// Originating repository shared by the commit and cleanliness result.
    pub fn repository_root(&self) -> &Path {
        &self.repository_root
    }

    /// Commit at `HEAD` when this state was read.
    pub fn source_commit(&self) -> &str {
        &self.source_commit
    }

    /// Whole-worktree source state, including untracked non-ignored files.
    pub const fn state(&self) -> GitSourceState {
        self.state
    }
}

/// Discover the repository worktree root from `path`.
pub async fn discover_repository_root(path: &Path) -> Result<PathBuf, GitSourceError> {
    git_path_output(
        path,
        &["rev-parse", "--path-format=absolute", "--show-toplevel"],
        GitSourceErrorKind::Discover,
        "discover worktree root",
    )
    .await
}

/// Read `HEAD` and whole-worktree cleanliness from the same repository.
pub async fn read_status(repository_root: &Path) -> Result<GitSourceSnapshot, GitSourceError> {
    let status = git_output(
        repository_root,
        &[
            "status",
            "--porcelain=v2",
            "--branch",
            "--untracked-files=normal",
            "--ignored=no",
        ],
        GitSourceErrorKind::Inspect,
        "read whole-worktree state",
    )
    .await?;
    let source_commit = status
        .stdout
        .split(|byte| *byte == b'\n')
        .find_map(|line| line.strip_prefix(b"# branch.oid "))
        .filter(|commit| !commit.is_empty() && *commit != b"(initial)")
        .ok_or_else(|| {
            GitSourceError::output(
                GitSourceErrorKind::Inspect,
                "read HEAD",
                repository_root,
                "Git status did not return a committed HEAD",
            )
        })?;
    let source_commit = std::str::from_utf8(source_commit).map_err(|_| {
        GitSourceError::output(
            GitSourceErrorKind::Inspect,
            "read HEAD",
            repository_root,
            "Git returned a non-UTF-8 commit identity",
        )
    })?;
    let state = status
        .stdout
        .split(|byte| *byte == b'\n')
        .any(|line| !line.is_empty() && !line.starts_with(b"# "))
        .then_some(GitSourceState::Dirty)
        .unwrap_or(GitSourceState::Clean);
    Ok(GitSourceSnapshot {
        repository_root: repository_root.to_owned(),
        source_commit: source_commit.to_owned().into_boxed_str(),
        state,
    })
}

async fn git_path_output(
    repository: &Path,
    args: &[&str],
    kind: GitSourceErrorKind,
    operation: &'static str,
) -> Result<PathBuf, GitSourceError> {
    let output = git_output(repository, args, kind, operation).await?;
    let bytes = one_output_line(&output.stdout, kind, operation, repository)?;
    Ok(PathBuf::from(OsString::from_vec(bytes.to_vec())))
}

async fn git_output(
    repository: &Path,
    args: &[&str],
    kind: GitSourceErrorKind,
    operation: &'static str,
) -> Result<Output, GitSourceError> {
    let output = Command::new("git")
        .arg("--no-optional-locks")
        .arg("-C")
        .arg(repository)
        .args(args)
        .kill_on_drop(true)
        .output()
        .await
        .map_err(|source| GitSourceError::io(kind, operation, repository, source))?;
    if output.status.success() {
        Ok(output)
    } else {
        Err(GitSourceError::command(
            kind, operation, repository, &output,
        ))
    }
}

fn one_output_line<'a>(
    output: &'a [u8],
    kind: GitSourceErrorKind,
    operation: &'static str,
    repository: &Path,
) -> Result<&'a [u8], GitSourceError> {
    let line = trim_ascii(output);
    if line.is_empty() || line.contains(&b'\n') || line.contains(&b'\r') {
        Err(GitSourceError::output(
            kind,
            operation,
            repository,
            "Git did not return exactly one nonempty line",
        ))
    } else {
        Ok(line)
    }
}

fn trim_ascii(mut bytes: &[u8]) -> &[u8] {
    while bytes
        .last()
        .is_some_and(|byte| matches!(byte, b'\n' | b'\r'))
    {
        bytes = &bytes[..bytes.len() - 1];
    }
    bytes
}
