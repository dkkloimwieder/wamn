use super::{GitSourceError, GitSourceErrorType, Path, PathBuf};

pub(super) fn one_output_line<'a>(
    output: &'a [u8],
    type_: GitSourceErrorType,
    operation: &'static str,
    repository: &Path,
) -> Result<&'a [u8], GitSourceError> {
    let line = trim_ascii(output);
    if line.is_empty() || line.contains(&b'\n') || line.contains(&b'\r') {
        Err(GitSourceError::output(
            type_,
            operation,
            repository,
            "Git did not return exactly one nonempty line",
        ))
    } else {
        Ok(line)
    }
}

pub(super) fn trim_ascii(mut bytes: &[u8]) -> &[u8] {
    while bytes
        .last()
        .is_some_and(|byte| matches!(byte, b'\n' | b'\r'))
    {
        bytes = &bytes[..bytes.len() - 1];
    }
    bytes
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
    pub(super) repository_root: PathBuf,
    pub(super) source_commit: Box<str>,
    pub(super) state: GitSourceState,
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
