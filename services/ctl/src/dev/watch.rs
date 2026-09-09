//! Git source-state and filesystem invalidation adapters for `wamn dev`.
//!
//! The adapter maps package-owned inputs and explicit component source roots
//! into engine stage identities. It deliberately does not coalesce events or
//! execute stages; [`super::run_watch`] remains the sole orchestration owner.

use std::collections::{BTreeSet, HashMap, VecDeque};
use std::error::Error;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::fs;
use std::io;
use std::mem::MaybeUninit;
use std::os::fd::OwnedFd;
use std::os::unix::ffi::{OsStrExt as _, OsStringExt as _};
use std::path::{Component, Path, PathBuf};
use std::process::{Output, Stdio};

use rustix::fs::inotify::{self, CreateFlags, ReadFlags, WatchFlags};
use tokio::io::AsyncWriteExt as _;
use tokio::io::unix::AsyncFd;
use tokio::process::Command;
use wamn_schema_generator::PackageManifest;

use super::{
    DevInvalidation, DevInvalidationSource, DevSourceState, DevSourceStateProvider, DevStage,
};

const INOTIFY_BUFFER_BYTES: usize = 64 * 1024;

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
    fn io(
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

    fn command(
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

    fn output(
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

/// One repository-grained source observation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GitSourceSnapshot {
    repository_root: PathBuf,
    source_commit: Box<str>,
    state: DevSourceState,
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
    pub const fn state(&self) -> DevSourceState {
        self.state
    }
}

/// Production Git adapter pinned to one originating repository.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GitSource {
    repository_root: PathBuf,
    git_dir: PathBuf,
    metadata_paths: Box<[PathBuf]>,
}

impl GitSource {
    /// Discover the repository and its exact worktree metadata from `path`.
    pub async fn discover(path: impl AsRef<Path>) -> Result<Self, GitSourceError> {
        let path = path.as_ref();
        let repository_root = git_path_output(
            path,
            &["rev-parse", "--path-format=absolute", "--show-toplevel"],
            GitSourceErrorKind::Discover,
            "discover worktree root",
        )
        .await?;
        let git_dir = git_path_output(
            &repository_root,
            &["rev-parse", "--absolute-git-dir"],
            GitSourceErrorKind::Discover,
            "discover worktree Git directory",
        )
        .await?;
        let metadata_paths = discover_metadata_paths(&repository_root, &git_dir).await?;

        Ok(Self {
            repository_root,
            git_dir,
            metadata_paths: metadata_paths.into_boxed_slice(),
        })
    }

    /// Stable root of the originating worktree.
    pub fn repository_root(&self) -> &Path {
        &self.repository_root
    }

    /// Read `HEAD` and whole-worktree cleanliness from the same repository.
    pub async fn snapshot(&self) -> Result<GitSourceSnapshot, GitSourceError> {
        let status = git_output(
            &self.repository_root,
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
                    &self.repository_root,
                    "Git status did not return a committed HEAD",
                )
            })?;
        let source_commit = std::str::from_utf8(source_commit).map_err(|_| {
            GitSourceError::output(
                GitSourceErrorKind::Inspect,
                "read HEAD",
                &self.repository_root,
                "Git returned a non-UTF-8 commit identity",
            )
        })?;
        let state = status
            .stdout
            .split(|byte| *byte == b'\n')
            .any(|line| !line.is_empty() && !line.starts_with(b"# "))
            .then_some(DevSourceState::Dirty)
            .unwrap_or(DevSourceState::Clean);
        Ok(GitSourceSnapshot {
            repository_root: self.repository_root.clone(),
            source_commit: source_commit.to_owned().into_boxed_str(),
            state,
        })
    }

    async fn refresh_metadata_paths(&mut self) -> Result<(), GitSourceError> {
        self.metadata_paths = discover_metadata_paths(&self.repository_root, &self.git_dir)
            .await?
            .into_boxed_slice();
        Ok(())
    }

    fn metadata_paths(&self) -> &[PathBuf] {
        &self.metadata_paths
    }

    fn head_path(&self) -> PathBuf {
        self.git_dir.join("HEAD")
    }
}

impl DevSourceStateProvider for GitSource {
    type Error = GitSourceError;

    async fn source_state(&mut self) -> Result<DevSourceState, Self::Error> {
        self.snapshot().await.map(|snapshot| snapshot.state())
    }
}

/// The commit-metadata paths this worktree may watch: `HEAD`, the `HEAD`
/// reflog beside it, the current branch ref, and the packed refs that ref can
/// be folded into. Which of them survive is decided by the retain rule below.
///
/// Two paths Git will happily name are deliberately absent.
///
/// The INDEX is not among them. It carries nothing about the source that the
/// rest miss: a commit moves `HEAD` or the current ref, a checkout moves both
/// of those and the working tree, and staging alone changes neither the
/// commit nor the cleanliness [`GitSource::snapshot`] reports. What it does
/// carry is a false positive. A plain `git status` typed in the worktree
/// rewrites the index whenever a cached stat is stale, and that cost the
/// author a whole rerun. The loop's own status reads pass
/// `--no-optional-locks` and never wrote it, so watching the refs instead
/// loses no invalidation the loop was acting on.
///
/// The other absence is the SHARED Git directory, and this is where the rule
/// changed (wamn-10yt.71, superseding wamn-10yt.60). A path is kept when it
/// is inside the worktree root OR inside this worktree's own Git directory,
/// the one `--absolute-git-dir` names, and dropped otherwise. That directory
/// is safe precisely because it is private: in a LINKED worktree it is
/// `.git/worktrees/<name>`, which no sibling checkout writes, while the
/// common directory it sits under — `--git-common-dir`, where `--git-path
/// packed-refs` and `--git-path refs/<branch>` land — is written by every
/// sibling, and watching it let one checkout's rebase invalidate an unrelated
/// session's loop. In a normal checkout the two directories are the same path
/// inside the worktree, so the added arm admits nothing new there.
///
/// A LINKED worktree therefore keeps `HEAD` and `logs/HEAD` and drops
/// `packed-refs` and `refs/<branch>`, which resolve into the common directory
/// it must not watch. The REFLOG is what makes that enough: a commit on an
/// attached branch moves only the shared branch ref, but it appends to the
/// private `logs/HEAD`, which a checkout and a reset also write and a plain
/// `git status` does not — the same property that keeps the index out. The
/// index is inside the private directory too, so the rule names and drops it
/// for the reason above.
///
/// That witness is configurable, so it is checked rather than assumed:
/// `core.logAllRefUpdates=false` stops Git creating the reflog at all, and
/// [`FilesystemInvalidationSource::new`] refuses to start watch mode there,
/// naming the setting. It refuses on the setting rather than probing for the
/// file, because an existing reflog is still appended to while a worktree
/// created under that setting gets none.
async fn discover_metadata_paths(
    repository_root: &Path,
    git_dir: &Path,
) -> Result<Vec<PathBuf>, GitSourceError> {
    let packed_refs = git_path_output(
        repository_root,
        &[
            "rev-parse",
            "--path-format=absolute",
            "--git-path",
            "packed-refs",
        ],
        GitSourceErrorKind::Discover,
        "discover packed Git refs",
    )
    .await?;
    let mut paths = BTreeSet::from([git_dir.join("HEAD"), git_dir.join("logs/HEAD"), packed_refs]);
    let symbolic = git_output_allowing_detached(
        repository_root,
        &["symbolic-ref", "-q", "HEAD"],
        GitSourceErrorKind::Discover,
        "discover current Git ref",
    )
    .await?;
    if let Some(symbolic) = symbolic {
        let symbolic = one_utf8_line(
            &symbolic.stdout,
            GitSourceErrorKind::Discover,
            "discover current Git ref",
            repository_root,
        )?;
        let reference = Path::new(&symbolic);
        if !reference.starts_with("refs")
            || !reference
                .components()
                .all(|component| matches!(component, Component::Normal(_)))
        {
            return Err(GitSourceError::output(
                GitSourceErrorKind::Discover,
                "discover current Git ref",
                repository_root,
                "symbolic HEAD is not a safe refs-relative path",
            ));
        }
        paths.insert(
            git_path_output(
                repository_root,
                &[
                    "rev-parse",
                    "--path-format=absolute",
                    "--git-path",
                    &symbolic,
                ],
                GitSourceErrorKind::Discover,
                "discover current Git ref path",
            )
            .await?,
        );
    }
    let index = git_dir.join("index");
    paths.retain(|path| {
        *path != index && (path.starts_with(repository_root) || path.starts_with(git_dir))
    });
    Ok(paths.into_iter().collect())
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

/// Ask the originating worktree which of `candidates` its ignore rules cover.
///
/// `check-ignore` reports a tracked path as not ignored, so this answers the
/// exact question the watcher has: is this directory the repository's own
/// output rather than authored source.
async fn git_ignored(
    repository: &Path,
    candidates: &[PathBuf],
) -> Result<BTreeSet<PathBuf>, GitSourceError> {
    const OPERATION: &str = "read Git ignore rules";
    if candidates.is_empty() {
        return Ok(BTreeSet::new());
    }
    let mut request = Vec::new();
    for candidate in candidates {
        request.extend_from_slice(candidate.as_os_str().as_bytes());
        request.push(0);
    }
    let mut child = Command::new("git")
        .arg("--no-optional-locks")
        .arg("-C")
        .arg(repository)
        .args(["check-ignore", "-z", "--stdin"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|source| {
            GitSourceError::io(GitSourceErrorKind::Inspect, OPERATION, repository, source)
        })?;
    let mut stdin = child.stdin.take().ok_or_else(|| {
        GitSourceError::output(
            GitSourceErrorKind::Inspect,
            OPERATION,
            repository,
            "Git did not accept a path list on standard input",
        )
    })?;
    // Written while the output is drained: a directory with more entries than
    // one pipe buffer would otherwise deadlock against Git's own writes.
    let (written, output) = tokio::join!(
        async move {
            stdin.write_all(&request).await?;
            stdin.shutdown().await
        },
        child.wait_with_output()
    );
    written.map_err(|source| {
        GitSourceError::io(GitSourceErrorKind::Inspect, OPERATION, repository, source)
    })?;
    let output = output.map_err(|source| {
        GitSourceError::io(GitSourceErrorKind::Inspect, OPERATION, repository, source)
    })?;
    match output.status.code() {
        // Git echoes each ignored path back exactly as it was given, so the
        // caller can match on the paths it supplied.
        Some(0) => Ok(output
            .stdout
            .split(|byte| *byte == 0)
            .filter(|path| !path.is_empty())
            .map(|path| PathBuf::from(OsString::from_vec(path.to_vec())))
            .collect()),
        Some(1) => Ok(BTreeSet::new()),
        _ => Err(GitSourceError::command(
            GitSourceErrorKind::Inspect,
            OPERATION,
            repository,
            &output,
        )),
    }
}

async fn git_output_allowing_detached(
    repository: &Path,
    args: &[&str],
    kind: GitSourceErrorKind,
    operation: &'static str,
) -> Result<Option<Output>, GitSourceError> {
    let output = Command::new("git")
        .arg("--no-optional-locks")
        .arg("-C")
        .arg(repository)
        .args(args)
        .kill_on_drop(true)
        .output()
        .await
        .map_err(|source| GitSourceError::io(kind, operation, repository, source))?;
    match output.status.code() {
        Some(0) => Ok(Some(output)),
        Some(1) => Ok(None),
        _ => Err(GitSourceError::command(
            kind, operation, repository, &output,
        )),
    }
}

fn one_utf8_line(
    output: &[u8],
    kind: GitSourceErrorKind,
    operation: &'static str,
    repository: &Path,
) -> Result<String, GitSourceError> {
    let line = one_output_line(output, kind, operation, repository)?;
    String::from_utf8(line.to_vec()).map_err(|_| {
        GitSourceError::output(
            kind,
            operation,
            repository,
            "Git returned non-UTF-8 identity output",
        )
    })
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

/// Stable category of a filesystem invalidation failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FilesystemInvalidationErrorKind {
    PackageRoot,
    ComponentRoot,
    Watch,
    Read,
    Git,
}

/// Failure to configure or read the production filesystem invalidation source.
#[derive(Debug)]
pub struct FilesystemInvalidationError {
    kind: FilesystemInvalidationErrorKind,
    path: PathBuf,
    detail: Box<str>,
    source: Option<Box<dyn Error + Send + Sync>>,
}

impl FilesystemInvalidationError {
    fn new(
        kind: FilesystemInvalidationErrorKind,
        path: impl Into<PathBuf>,
        detail: impl Into<Box<str>>,
    ) -> Self {
        Self {
            kind,
            path: path.into(),
            detail: detail.into(),
            source: None,
        }
    }

    fn with_source(
        kind: FilesystemInvalidationErrorKind,
        path: impl Into<PathBuf>,
        detail: impl Into<Box<str>>,
        source: impl Error + Send + Sync + 'static,
    ) -> Self {
        Self {
            kind,
            path: path.into(),
            detail: detail.into(),
            source: Some(Box::new(source)),
        }
    }

    /// Stable error category.
    pub const fn kind(&self) -> FilesystemInvalidationErrorKind {
        self.kind
    }
}

impl fmt::Display for FilesystemInvalidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} at {}", self.detail, self.path.display())?;
        if let Some(source) = &self.source {
            write!(formatter, ": {source}")?;
        }
        Ok(())
    }
}

impl Error for FilesystemInvalidationError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        self.source
            .as_deref()
            .map(|source| source as &(dyn Error + 'static))
    }
}

#[derive(Debug)]
struct PackageRoot {
    root: PathBuf,
    authored_inputs: BTreeSet<PathBuf>,
}

impl PackageRoot {
    fn read(root: &Path) -> Result<Self, FilesystemInvalidationError> {
        let root = root.canonicalize().map_err(|source| {
            FilesystemInvalidationError::with_source(
                FilesystemInvalidationErrorKind::PackageRoot,
                root,
                "cannot resolve package root",
                source,
            )
        })?;
        let manifest_path = root.join("wamn.json");
        let manifest_bytes = fs::read(&manifest_path).map_err(|source| {
            FilesystemInvalidationError::with_source(
                FilesystemInvalidationErrorKind::PackageRoot,
                &manifest_path,
                "cannot read package manifest",
                source,
            )
        })?;
        let manifest = PackageManifest::from_slice(&manifest_bytes).map_err(|source| {
            FilesystemInvalidationError::with_source(
                FilesystemInvalidationErrorKind::PackageRoot,
                &manifest_path,
                "cannot parse package manifest",
                source,
            )
        })?;
        Ok(Self {
            authored_inputs: authored_inputs(&root, &manifest),
            root,
        })
    }

    fn refresh_authored_inputs(&mut self) {
        let manifest_path = self.root.join("wamn.json");
        let Ok(bytes) = fs::read(manifest_path) else {
            return;
        };
        let Ok(manifest) = PackageManifest::from_slice(&bytes) else {
            return;
        };
        self.authored_inputs = authored_inputs(&self.root, &manifest);
    }

    fn stage(&self, path: &Path) -> Option<DevStage> {
        let relative = path.strip_prefix(&self.root).ok()?;
        if relative.as_os_str().is_empty()
            || relative == Path::new("wamn.json")
            || relative.starts_with("migrations")
        {
            return Some(DevStage::Migrate);
        }
        if relative.starts_with("generated") {
            return None;
        }
        if self.authored_inputs.contains(path) {
            return Some(DevStage::Generate);
        }
        if relative.starts_with("publication/components") {
            return Some(DevStage::Admit);
        }
        if relative.starts_with("publication/wirings") {
            return Some(DevStage::Gate);
        }
        if relative == Path::new("publication/attachments.json") {
            return Some(DevStage::Release);
        }
        None
    }

    fn owns_generated(&self, path: &Path) -> bool {
        path.strip_prefix(&self.root)
            .is_ok_and(|relative| relative.starts_with("generated"))
    }
}

fn authored_inputs(root: &Path, manifest: &PackageManifest) -> BTreeSet<PathBuf> {
    let mut paths = manifest
        .models
        .values()
        .flat_map(|model| model.operations.values())
        .filter_map(|operation| operation.authored_sql.as_ref())
        .flat_map(|authored| {
            std::iter::once(authored.default.as_str()).chain(
                authored
                    .variants
                    .iter()
                    .map(|variant| variant.path.as_str()),
            )
        })
        .filter(|path| is_authored_input(path))
        .map(|path| root.join(path))
        .collect::<BTreeSet<_>>();
    paths.extend(
        manifest
            .custom_operations
            .values()
            .flat_map(|operation| operation.statements.values())
            .filter(|statement| is_authored_input(&statement.path))
            .map(|statement| root.join(&statement.path)),
    );
    paths
}

fn is_authored_input(path: impl AsRef<Path>) -> bool {
    let path = path.as_ref();
    (path.starts_with("query") || path.starts_with("command"))
        && path.extension().is_some_and(|extension| extension == "sql")
        && path
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
}

#[derive(Debug)]
struct WatchRoots {
    packages: Vec<PackageRoot>,
    component_build_roots: Box<[PathBuf]>,
    git_metadata: BTreeSet<PathBuf>,
    excluded: BTreeSet<PathBuf>,
}

impl WatchRoots {
    fn new(
        package_roots: impl IntoIterator<Item = PathBuf>,
        component_build_roots: impl IntoIterator<Item = PathBuf>,
        git: &GitSource,
    ) -> Result<Self, FilesystemInvalidationError> {
        let packages = package_roots
            .into_iter()
            .map(|root| PackageRoot::read(&root))
            .collect::<Result<Vec<_>, _>>()?;
        for package in &packages {
            if !package.root.starts_with(git.repository_root()) {
                return Err(FilesystemInvalidationError::new(
                    FilesystemInvalidationErrorKind::PackageRoot,
                    &package.root,
                    "package root is outside the originating Git worktree",
                ));
            }
        }
        let component_build_roots = component_build_roots
            .into_iter()
            .map(|root| {
                root.canonicalize().map_err(|source| {
                    FilesystemInvalidationError::with_source(
                        FilesystemInvalidationErrorKind::ComponentRoot,
                        &root,
                        "cannot resolve component build root",
                        source,
                    )
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        for root in &component_build_roots {
            if !root.starts_with(git.repository_root()) {
                return Err(FilesystemInvalidationError::new(
                    FilesystemInvalidationErrorKind::ComponentRoot,
                    root,
                    "component build root is outside the originating Git worktree",
                ));
            }
        }
        Ok(Self {
            packages,
            component_build_roots: component_build_roots.into_boxed_slice(),
            git_metadata: git.metadata_paths().iter().cloned().collect(),
            excluded: BTreeSet::new(),
        })
    }

    /// Record one directory the ignore rules put outside the watch.
    fn exclude(&mut self, directory: PathBuf) {
        self.excluded.insert(directory);
    }

    fn is_excluded(&self, path: &Path) -> bool {
        self.excluded
            .iter()
            .any(|directory| path == directory || path.starts_with(directory))
    }

    fn stage(&self, path: &Path) -> Option<DevStage> {
        // An excluded directory is unwatched, so nothing inside it can arrive
        // here; the directory ITSELF still can, through its watched parent,
        // when a build creates or replaces it.
        if self.is_excluded(path) {
            return None;
        }
        if self
            .packages
            .iter()
            .any(|package| package.owns_generated(path))
        {
            return None;
        }
        let package_stage = self
            .packages
            .iter()
            .filter_map(|package| package.stage(path))
            .min_by_key(|stage| stage.position());
        let component_stage = self
            .component_build_roots
            .iter()
            .any(|root| path == root || path.starts_with(root))
            .then_some(DevStage::Build);
        package_stage
            .into_iter()
            .chain(component_stage)
            .min_by_key(|stage| stage.position())
    }

    /// An ANCESTOR of a metadata path counts, and that is deliberate.
    ///
    /// A metadata path need not exist yet, so the watch is placed on its
    /// nearest existing ancestor directory instead; the first thing that
    /// arrives when Git writes a ref under a branch prefix is the CREATE of
    /// the intermediate directory, named by that ancestor and not by the ref.
    /// Ancestors above the watched directory are never an event subject, so
    /// the rule reaches no further than the window it exists for.
    fn is_git_metadata(&self, path: &Path) -> bool {
        self.git_metadata
            .iter()
            .any(|metadata| metadata == path || metadata.starts_with(path))
    }

    fn refresh_manifest(&mut self, path: &Path) {
        for package in &mut self.packages {
            if path == package.root.join("wamn.json") {
                package.refresh_authored_inputs();
            }
        }
    }

    fn refresh_git_metadata(&mut self, git: &GitSource) {
        self.git_metadata = git.metadata_paths().iter().cloned().collect();
    }

    fn watched_roots(&self) -> impl Iterator<Item = &Path> {
        self.packages
            .iter()
            .map(|package| package.root.as_path())
            .chain(self.component_build_roots.iter().map(PathBuf::as_path))
    }
}

#[derive(Debug)]
struct RawChange {
    path: Option<PathBuf>,
    events: ReadFlags,
}

/// Linux filesystem invalidation source over explicit package and build roots.
pub struct FilesystemInvalidationSource {
    inotify: AsyncFd<OwnedFd>,
    watched_directories: HashMap<i32, PathBuf>,
    roots: WatchRoots,
    git: GitSource,
    pending: VecDeque<DevInvalidation>,
}

impl fmt::Debug for FilesystemInvalidationSource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FilesystemInvalidationSource")
            .field("watched_directories", &self.watched_directories.len())
            .field("roots", &self.roots)
            .field("git", &self.git)
            .field("pending", &self.pending.len())
            .finish()
    }
}

impl FilesystemInvalidationSource {
    /// Register recursive event watches for exact roots supplied by the caller.
    ///
    /// This constructor is watch mode's startup, and the only one: `wamn dev`
    /// builds it in its watch branch alone, so the reflog refusal below stops
    /// watch mode without touching a one-shot run, which needs no reflog
    /// because it never waits for a second commit.
    pub async fn new(
        package_roots: impl IntoIterator<Item = PathBuf>,
        component_build_roots: impl IntoIterator<Item = PathBuf>,
        git: GitSource,
    ) -> Result<Self, FilesystemInvalidationError> {
        require_head_reflog(git.repository_root()).await?;
        let roots = WatchRoots::new(package_roots, component_build_roots, &git)?;
        let descriptor =
            inotify::init(CreateFlags::CLOEXEC | CreateFlags::NONBLOCK).map_err(|source| {
                FilesystemInvalidationError::with_source(
                    FilesystemInvalidationErrorKind::Watch,
                    git.repository_root(),
                    "cannot initialize filesystem notification",
                    errno_to_io(source),
                )
            })?;
        let inotify = AsyncFd::new(descriptor).map_err(|source| {
            FilesystemInvalidationError::with_source(
                FilesystemInvalidationErrorKind::Watch,
                git.repository_root(),
                "cannot register filesystem notification with the async runtime",
                source,
            )
        })?;
        let mut source = Self {
            inotify,
            watched_directories: HashMap::new(),
            roots,
            git,
            pending: VecDeque::new(),
        };
        let watched_roots = source
            .roots
            .watched_roots()
            .map(Path::to_owned)
            .collect::<Vec<_>>();
        for root in watched_roots {
            source.add_tree(&root).await?;
        }
        let metadata_parents = metadata_watch_directories(source.git.metadata_paths());
        for parent in metadata_parents {
            source.add_directory(&parent)?;
        }
        Ok(source)
    }

    /// Register `root` and every directory beneath it that Git does not ignore.
    ///
    /// The ignore rules are the filter because the loop's own outputs are
    /// exactly what they cover: the Build stage writes into
    /// `components/target/`, which lies inside a component build root, so a
    /// watch that descended there invalidated on its own artifacts and reran
    /// forever with no edit at all (wamn-10yt.55). Ignored bytes are already
    /// not source by this module's other definition — [`GitSource::snapshot`]
    /// reads `--ignored=no` — so one rule now decides both questions.
    async fn add_tree(&mut self, root: &Path) -> Result<(), FilesystemInvalidationError> {
        let mut pending = self.retain_watchable(vec![root.to_owned()]).await?;
        while let Some(directory) = pending.pop() {
            self.add_directory(&directory)?;
            let children = read_subdirectories(&directory)?;
            pending.extend(self.retain_watchable(children).await?);
        }
        Ok(())
    }

    /// Drop the ignored candidates, remembering each as permanently unwatched.
    ///
    /// Only paths inside the worktree are put to Git. The commit-metadata
    /// watches deliberately sit in the Git directory, and a LINKED worktree's
    /// Git directory lives in the main checkout, outside this repository
    /// entirely; asking Git about a path out there is a fatal refusal rather
    /// than an answer. Nothing there is ignored, so nothing needs asking.
    async fn retain_watchable(
        &mut self,
        candidates: Vec<PathBuf>,
    ) -> Result<Vec<PathBuf>, FilesystemInvalidationError> {
        let inside = candidates
            .iter()
            .filter(|candidate| candidate.starts_with(self.git.repository_root()))
            .cloned()
            .collect::<Vec<_>>();
        let ignored = git_ignored(self.git.repository_root(), &inside)
            .await
            .map_err(|source| {
                FilesystemInvalidationError::with_source(
                    FilesystemInvalidationErrorKind::Git,
                    self.git.repository_root(),
                    "cannot read the ignore rules that bound the watch",
                    source,
                )
            })?;
        for directory in &ignored {
            self.roots.exclude(directory.clone());
        }
        Ok(candidates
            .into_iter()
            .filter(|candidate| !ignored.contains(candidate))
            .collect())
    }

    fn add_directory(&mut self, directory: &Path) -> Result<(), FilesystemInvalidationError> {
        let descriptor = inotify::add_watch(
            self.inotify.get_ref(),
            directory,
            WatchFlags::ATTRIB
                | WatchFlags::CLOSE_WRITE
                | WatchFlags::CREATE
                | WatchFlags::DELETE
                | WatchFlags::DELETE_SELF
                | WatchFlags::MOVED_FROM
                | WatchFlags::MOVED_TO
                | WatchFlags::MOVE_SELF
                | WatchFlags::ONLYDIR,
        )
        .map_err(|source| {
            FilesystemInvalidationError::with_source(
                FilesystemInvalidationErrorKind::Watch,
                directory,
                "cannot watch directory",
                errno_to_io(source),
            )
        })?;
        self.watched_directories
            .insert(descriptor, directory.to_owned());
        Ok(())
    }

    async fn read_batch(&mut self) -> Result<(), FilesystemInvalidationError> {
        let changes = loop {
            let mut readiness = self.inotify.readable().await.map_err(|source| {
                FilesystemInvalidationError::with_source(
                    FilesystemInvalidationErrorKind::Read,
                    self.git.repository_root(),
                    "cannot await filesystem notification",
                    source,
                )
            })?;
            match readiness
                .try_io(|descriptor| read_changes(descriptor.get_ref(), &self.watched_directories))
            {
                Ok(changes) => {
                    break changes.map_err(|source| {
                        FilesystemInvalidationError::with_source(
                            FilesystemInvalidationErrorKind::Read,
                            self.git.repository_root(),
                            "cannot read filesystem notification",
                            source,
                        )
                    })?;
                }
                Err(_would_block) => continue,
            }
        };

        let mut head_changed = false;
        for change in &changes {
            if change.events.contains(ReadFlags::QUEUE_OVERFLOW) {
                continue;
            }
            let Some(path) = &change.path else {
                continue;
            };
            if change.events.contains(ReadFlags::ISDIR)
                && change
                    .events
                    .intersects(ReadFlags::CREATE | ReadFlags::MOVED_TO)
                && path.is_dir()
            {
                self.add_tree(path).await?;
            }
            self.roots.refresh_manifest(path);
            head_changed |= path == &self.git.head_path();
        }
        if head_changed {
            self.git.refresh_metadata_paths().await.map_err(|source| {
                FilesystemInvalidationError::with_source(
                    FilesystemInvalidationErrorKind::Git,
                    self.git.repository_root(),
                    "cannot refresh current Git metadata watch",
                    source,
                )
            })?;
            self.roots.refresh_git_metadata(&self.git);
            let parents = metadata_watch_directories(self.git.metadata_paths());
            for parent in parents {
                self.add_directory(&parent)?;
            }
        }

        let needs_source_state = changes.iter().any(|change| {
            change.events.contains(ReadFlags::QUEUE_OVERFLOW)
                || change.path.as_deref().is_some_and(|path| {
                    self.roots.stage(path).is_some() || self.roots.is_git_metadata(path)
                })
        });
        let source_state = if needs_source_state {
            self.git
                .snapshot()
                .await
                .map_err(|source| {
                    FilesystemInvalidationError::with_source(
                        FilesystemInvalidationErrorKind::Git,
                        self.git.repository_root(),
                        "cannot read source state for filesystem invalidation",
                        source,
                    )
                })?
                .state()
        } else {
            DevSourceState::Clean
        };

        for change in changes {
            let invalidation = if change.events.contains(ReadFlags::QUEUE_OVERFLOW) {
                DevInvalidation::Rerun {
                    from: DevStage::Migrate,
                    source_state,
                }
            } else if let Some(path) = change.path {
                // Path classification decides RELEVANCE, not the starting
                // stage. Every relevant change reruns the whole pipeline,
                // because the target database is recreated per run and a
                // suffix that started after Apply would run against an empty
                // one. What a run does not have to redo is decided by each
                // stage's input digest, not by which file was touched.
                if self.roots.is_git_metadata(&path) || self.roots.stage(&path).is_some() {
                    DevInvalidation::Rerun {
                        from: DevStage::Migrate,
                        source_state,
                    }
                } else {
                    DevInvalidation::Ignore
                }
            } else {
                DevInvalidation::Ignore
            };
            self.pending.push_back(invalidation);
        }
        Ok(())
    }
}

impl DevInvalidationSource for FilesystemInvalidationSource {
    type Error = FilesystemInvalidationError;

    async fn next(&mut self) -> Result<Option<DevInvalidation>, Self::Error> {
        if let Some(invalidation) = self.pending.pop_front() {
            return Ok(Some(invalidation));
        }
        self.read_batch().await?;
        Ok(self.pending.pop_front())
    }

    fn try_next(&mut self) -> Result<Option<DevInvalidation>, Self::Error> {
        Ok(self.pending.pop_front())
    }
}

fn read_changes(
    descriptor: &OwnedFd,
    watched_directories: &HashMap<i32, PathBuf>,
) -> io::Result<Vec<RawChange>> {
    let mut buffer = [MaybeUninit::uninit(); INOTIFY_BUFFER_BYTES];
    let mut reader = inotify::Reader::new(descriptor, &mut buffer);
    let mut changes = Vec::new();
    loop {
        match reader.next() {
            Ok(event) => {
                let path = watched_directories.get(&event.wd()).map(|directory| {
                    event.file_name().map_or_else(
                        || directory.clone(),
                        |name| directory.join(OsStr::from_bytes(name.to_bytes())),
                    )
                });
                changes.push(RawChange {
                    path,
                    events: event.events(),
                });
            }
            Err(rustix::io::Errno::AGAIN) if changes.is_empty() => {
                return Err(io::ErrorKind::WouldBlock.into());
            }
            Err(rustix::io::Errno::AGAIN) => return Ok(changes),
            Err(source) => return Err(errno_to_io(source)),
        }
    }
}

fn read_subdirectories(directory: &Path) -> Result<Vec<PathBuf>, FilesystemInvalidationError> {
    let entries = fs::read_dir(directory).map_err(|source| {
        FilesystemInvalidationError::with_source(
            FilesystemInvalidationErrorKind::Watch,
            directory,
            "cannot enumerate watched directory",
            source,
        )
    })?;
    let mut children = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|source| {
            FilesystemInvalidationError::with_source(
                FilesystemInvalidationErrorKind::Watch,
                directory,
                "cannot read watched directory entry",
                source,
            )
        })?;
        let file_type = entry.file_type().map_err(|source| {
            FilesystemInvalidationError::with_source(
                FilesystemInvalidationErrorKind::Watch,
                entry.path(),
                "cannot inspect watched directory entry",
                source,
            )
        })?;
        if file_type.is_dir() && !file_type.is_symlink() {
            children.push(entry.path());
        }
    }
    Ok(children)
}

/// Refuse watch mode when the repository has turned the `HEAD` reflog off.
///
/// `logs/HEAD` is the only per-worktree witness of a commit on an attached
/// branch, so `core.logAllRefUpdates=false` would leave the loop watching a
/// file Git never creates and losing commit detection in silence. Naming the
/// setting turns that into something an operator can act on without reading
/// this file. Git's default for a non-bare repository is on, so the common
/// case pays one `git config` read and nothing else.
///
/// Only an explicit boolean false disables it, so only a zero exit reporting
/// `false` refuses. Every non-zero exit means it is not off: 1 is unset, and
/// 128 is a non-boolean value such as `always`, which logs more, not less.
async fn require_head_reflog(repository_root: &Path) -> Result<(), FilesystemInvalidationError> {
    let output = Command::new("git")
        .arg("--no-optional-locks")
        .arg("-C")
        .arg(repository_root)
        .args(["config", "--bool", "--get", "core.logAllRefUpdates"])
        .kill_on_drop(true)
        .output()
        .await
        .map_err(|source| {
            FilesystemInvalidationError::with_source(
                FilesystemInvalidationErrorKind::Git,
                repository_root,
                "cannot read core.logAllRefUpdates",
                source,
            )
        })?;
    if output.status.success() && trim_ascii(&output.stdout) == b"false" {
        return Err(FilesystemInvalidationError::new(
            FilesystemInvalidationErrorKind::Git,
            repository_root,
            "core.logAllRefUpdates is false, so Git writes no HEAD reflog and a \
             commit would not rerun the loop; set core.logAllRefUpdates=true to \
             watch this repository",
        ));
    }
    Ok(())
}

fn metadata_watch_directories(paths: &[PathBuf]) -> BTreeSet<PathBuf> {
    paths
        .iter()
        .filter_map(|path| {
            let mut candidate = path.parent();
            while let Some(directory) = candidate {
                if directory.is_dir() {
                    return Some(directory.to_owned());
                }
                candidate = directory.parent();
            }
            None
        })
        .collect()
}

fn errno_to_io(source: rustix::io::Errno) -> io::Error {
    source.into()
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::Duration;

    use super::*;

    static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

    struct TempRepository {
        root: PathBuf,
    }

    impl TempRepository {
        fn new() -> Self {
            let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let root = std::env::temp_dir()
                .join(format!("wamn-dev-watch-{}-{sequence}", std::process::id()));
            fs::create_dir(&root).expect("create temporary repository");
            git(&root, &["init", "--quiet"]);
            git(
                &root,
                &["config", "user.email", "dev-watch@example.invalid"],
            );
            git(&root, &["config", "user.name", "Dev Watch Test"]);
            Self { root }
        }

        fn package(&self) -> PathBuf {
            self.root.join("package")
        }

        fn component(&self) -> PathBuf {
            self.root.join("component")
        }

        fn write_fixture(&self) {
            fs::create_dir_all(self.package().join("query")).expect("create package query root");
            fs::create_dir_all(self.package().join("generated")).expect("create generated root");
            fs::create_dir_all(self.package().join("publication/components"))
                .expect("create component declarations root");
            fs::create_dir_all(self.package().join("publication/wirings"))
                .expect("create wiring declarations root");
            fs::create_dir_all(self.package().join("migrations")).expect("create migrations root");
            fs::create_dir_all(self.component().join("src")).expect("create component source root");
            fs::write(
                self.package().join("wamn.json"),
                include_bytes!("../../../../packages/receiving/wamn.json"),
            )
            .expect("write package manifest");
            fs::write(
                self.package().join("query/open_purchase_order.sql"),
                "SELECT 1",
            )
            .expect("write authored SQL");
            fs::write(self.component().join("src/lib.rs"), "pub fn value() {}")
                .expect("write component source");
            fs::write(self.root.join(".gitignore"), "ignored\n").expect("write ignore rules");
            git(&self.root, &["add", "."]);
            git(&self.root, &["commit", "--quiet", "-m", "fixture"]);
        }
    }

    impl Drop for TempRepository {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.root).expect("remove temporary repository");
        }
    }

    fn git(root: &Path, args: &[&str]) {
        let status = std::process::Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .status()
            .expect("execute fixture Git command");
        assert!(status.success(), "fixture Git command failed: {args:?}");
    }

    fn collect_batch(
        first: DevInvalidation,
        source: &mut FilesystemInvalidationSource,
    ) -> Vec<DevInvalidation> {
        let mut events = vec![first];
        while let Some(event) = source.try_next().expect("drain filesystem events") {
            events.push(event);
        }
        events
    }

    fn has_rerun(
        events: &[DevInvalidation],
        expected_stage: DevStage,
        expected_state: DevSourceState,
    ) -> bool {
        events.iter().any(|event| {
            matches!(
                event,
                DevInvalidation::Rerun { from, source_state }
                    if *from == expected_stage && *source_state == expected_state
            )
        })
    }

    #[test]
    fn package_artifacts_map_to_their_first_semantic_owner() {
        let repository = TempRepository::new();
        repository.write_fixture();
        let package = PackageRoot::read(&repository.package()).expect("read package watch root");
        let root = repository.package();

        assert_eq!(
            package.stage(&root.join("wamn.json")),
            Some(DevStage::Migrate)
        );
        assert_eq!(
            package.stage(&root.join("migrations/0002.sql")),
            Some(DevStage::Migrate)
        );
        assert_eq!(
            package.stage(&root.join("query/open_purchase_order.sql")),
            Some(DevStage::Generate)
        );
        assert_eq!(package.stage(&root.join("query/not-declared.sql")), None);
        assert_eq!(package.stage(&root.join("generated/wamn.rs")), None);
        assert_eq!(
            package.stage(&root.join("publication/components/receiving.json.in")),
            Some(DevStage::Admit)
        );
        assert_eq!(
            package.stage(&root.join("publication/wirings/receiving.json")),
            Some(DevStage::Gate)
        );
        assert_eq!(
            package.stage(&root.join("publication/attachments.json")),
            Some(DevStage::Release)
        );
        assert_eq!(package.stage(&root.join("README.md")), None);
    }

    #[tokio::test]
    async fn git_snapshot_covers_the_whole_worktree_and_excludes_ignored_files() {
        let repository = TempRepository::new();
        repository.write_fixture();
        let source = GitSource::discover(repository.package())
            .await
            .expect("discover source repository");

        let clean = source.snapshot().await.expect("read clean source state");
        assert_eq!(clean.repository_root(), repository.root);
        assert_eq!(clean.state(), DevSourceState::Clean);
        assert!(!clean.source_commit().is_empty());

        fs::write(repository.root.join("ignored"), "ignored bytes").expect("write ignored file");
        assert_eq!(
            source.snapshot().await.expect("read ignored state").state(),
            DevSourceState::Clean
        );

        let untracked = repository.root.join("outside-package");
        fs::write(&untracked, "untracked bytes").expect("write untracked file");
        assert_eq!(
            source
                .snapshot()
                .await
                .expect("read untracked state")
                .state(),
            DevSourceState::Dirty
        );
        fs::remove_file(untracked).expect("remove untracked file");
    }

    /// The Build stage writes inside a component build root, so a watch that
    /// descended into that output invalidated on its own artifacts and reran
    /// with no edit until it was stopped (wamn-10yt.55). Both orders matter:
    /// the output root can already exist when the session starts, and the
    /// first build of a fresh worktree creates it mid-session.
    #[tokio::test]
    async fn ignored_build_output_inside_a_component_root_never_reruns_the_loop() {
        let repository = TempRepository::new();
        repository.write_fixture();
        let output = repository.component().join("ignored");
        fs::create_dir(&output).expect("create the pre-existing build output root");
        let git_source = GitSource::discover(&repository.root)
            .await
            .expect("discover source repository");
        let mut source = FilesystemInvalidationSource::new(
            [repository.package()],
            [repository.component()],
            git_source,
        )
        .await
        .expect("construct filesystem invalidation source");

        fs::create_dir(output.join("wasm32-wasip2")).expect("create the build target directory");
        fs::write(output.join("wasm32-wasip2/guest.wasm"), "artifact")
            .expect("write a build artifact");
        assert!(
            tokio::time::timeout(Duration::from_millis(500), source.next())
                .await
                .is_err(),
            "an output root that existed at construction is never watched at all"
        );

        // Recreated mid-session the root is seen once, through its watched
        // parent, and must still classify as nothing to rerun.
        fs::remove_dir_all(&output).expect("remove the build output root");
        fs::create_dir(&output).expect("recreate the build output root");
        fs::write(output.join("guest.wasm"), "artifact").expect("write a rebuilt artifact");
        let first = tokio::time::timeout(Duration::from_secs(2), source.next())
            .await
            .expect("build output event arrived")
            .expect("read build output event")
            .expect("source remains open");
        assert!(
            collect_batch(first, &mut source)
                .iter()
                .all(|event| *event == DevInvalidation::Ignore),
            "a rebuilt output root must not invalidate the loop that wrote it"
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(500), source.next())
                .await
                .is_err(),
            "the recreated output root must be pruned again, not rewatched"
        );
    }

    /// The commit-metadata watches sit in the Git directory, and for a LINKED
    /// worktree that directory is in the main checkout, outside this
    /// repository. Git refuses to answer an ignore question about a path out
    /// there, and a watch that treated the refusal as a failure died mid
    /// session the first time another worktree created `.git/sequencer`.
    #[tokio::test]
    async fn a_directory_outside_the_worktree_is_watched_without_asking_git() {
        let repository = TempRepository::new();
        repository.write_fixture();
        let git_source = GitSource::discover(&repository.root)
            .await
            .expect("discover source repository");
        let mut source = FilesystemInvalidationSource::new(
            [repository.package()],
            [repository.component()],
            git_source,
        )
        .await
        .expect("construct filesystem invalidation source");

        let outside = repository.root.with_extension("outside");
        fs::create_dir(&outside).expect("create a directory outside the worktree");
        let watched = source.add_tree(&outside).await;
        fs::remove_dir_all(&outside).expect("remove the outside directory");
        watched.expect("a path Git cannot be asked about is still watchable");
    }

    #[tokio::test]
    async fn filesystem_events_map_owned_inputs_and_ignore_generated_outputs() {
        let repository = TempRepository::new();
        repository.write_fixture();
        let git_source = GitSource::discover(&repository.root)
            .await
            .expect("discover source repository");
        let mut source = FilesystemInvalidationSource::new(
            [repository.package()],
            [repository.component()],
            git_source,
        )
        .await
        .expect("construct filesystem invalidation source");

        fs::write(repository.package().join("generated/wamn.rs"), "generated")
            .expect("write generated output");
        let first = tokio::time::timeout(Duration::from_secs(2), source.next())
            .await
            .expect("generated event arrived")
            .expect("read generated event")
            .expect("source remains open");
        assert!(
            collect_batch(first, &mut source)
                .iter()
                .all(|event| *event == DevInvalidation::Ignore)
        );

        fs::write(
            repository.package().join("query/open_purchase_order.sql"),
            "SELECT 2",
        )
        .expect("edit authored SQL");
        let first = tokio::time::timeout(Duration::from_secs(2), source.next())
            .await
            .expect("authored SQL event arrived")
            .expect("read authored SQL event")
            .expect("source remains open");
        assert!(has_rerun(
            &collect_batch(first, &mut source),
            DevStage::Migrate,
            DevSourceState::Dirty
        ));

        fs::write(
            repository.component().join("src/lib.rs"),
            "pub fn next() {}",
        )
        .expect("edit component source");
        let first = tokio::time::timeout(Duration::from_secs(2), source.next())
            .await
            .expect("component event arrived")
            .expect("read component event")
            .expect("source remains open");
        assert!(has_rerun(
            &collect_batch(first, &mut source),
            DevStage::Migrate,
            DevSourceState::Dirty
        ));
    }

    #[tokio::test]
    async fn clean_commit_metadata_reruns_without_another_package_edit() {
        let repository = TempRepository::new();
        repository.write_fixture();
        let git_source = GitSource::discover(&repository.root)
            .await
            .expect("discover source repository");
        let mut source = FilesystemInvalidationSource::new(
            [repository.package()],
            [repository.component()],
            git_source,
        )
        .await
        .expect("construct filesystem invalidation source");

        fs::write(
            repository.package().join("query/open_purchase_order.sql"),
            "SELECT 3",
        )
        .expect("edit authored SQL");
        let dirty = tokio::time::timeout(Duration::from_secs(2), source.next())
            .await
            .expect("dirty package event arrived")
            .expect("read dirty package event")
            .expect("source remains open");
        let dirty = collect_batch(dirty, &mut source);
        assert!(has_rerun(&dirty, DevStage::Migrate, DevSourceState::Dirty));

        git(&repository.root, &["add", "."]);
        git(&repository.root, &["commit", "--quiet", "-m", "save edit"]);
        let committed = tokio::time::timeout(Duration::from_secs(2), source.next())
            .await
            .expect("commit metadata event arrived")
            .expect("read commit metadata event")
            .expect("source remains open");
        let committed = collect_batch(committed, &mut source);
        // The property this guards is that a commit is itself an
        // invalidation carrying CLEAN source state, so a run can reach the
        // committed-source stages without the author touching the package
        // again. Which stage it resumes at is no longer part of it: every
        // relevant change reruns the whole pipeline.
        assert!(has_rerun(
            &committed,
            DevStage::Migrate,
            DevSourceState::Clean
        ));
    }

    /// Every watch a loop registers sits inside the worktree it runs in or
    /// inside that worktree's own Git directory, and nowhere else.
    ///
    /// In a LINKED worktree `--git-path` answers with the MAIN checkout:
    /// `packed-refs` and the branch ref land in the COMMON directory every
    /// sibling worktree writes, `HEAD` and the index in the private
    /// `.git/worktrees/<name>` beside it. Watching the common directory let an
    /// unrelated checkout's rebase invalidate this session's loop; watching
    /// the private one cannot, and brings back `HEAD` and the `HEAD` reflog —
    /// which together see a commit on an attached branch (wamn-10yt.71).
    #[tokio::test]
    async fn a_linked_worktree_watches_its_own_git_directory_and_not_the_shared_one() {
        let repository = TempRepository::new();
        repository.write_fixture();
        let linked = repository.root.with_extension("linked");
        let linked_argument = linked.to_str().expect("linked worktree path is UTF-8");
        git(
            &repository.root,
            &[
                "worktree",
                "add",
                "--quiet",
                "-b",
                "linked",
                linked_argument,
            ],
        );

        let git_source = GitSource::discover(&linked)
            .await
            .expect("discover the linked worktree");
        let worktree_root = git_source.repository_root().to_owned();
        let mut source = FilesystemInvalidationSource::new(
            [linked.join("package")],
            [linked.join("component")],
            git_source,
        )
        .await
        .expect("construct filesystem invalidation source");
        let own_git_dir = source.git.git_dir.clone();
        let shared_git_dir = repository.root.join(".git");
        let outside = source
            .watched_directories
            .values()
            .filter(|directory| {
                !directory.starts_with(&worktree_root) && !directory.starts_with(&own_git_dir)
            })
            .cloned()
            .collect::<Vec<_>>();
        let shared = source
            .watched_directories
            .values()
            .filter(|directory| {
                directory.starts_with(&shared_git_dir) && !directory.starts_with(&own_git_dir)
            })
            .cloned()
            .collect::<Vec<_>>();
        let watches_own_git_dir = source
            .watched_directories
            .values()
            .any(|directory| directory.starts_with(&own_git_dir));
        let metadata = source.git.metadata_paths().to_vec();

        // The exact shape that killed a session: a rebase in the OTHER
        // checkout writing `.git/sequencer`, beside a status refreshing that
        // checkout's own index. Neither is this worktree's source.
        fs::create_dir(repository.root.join(".git/sequencer"))
            .expect("stage a rebase in the main checkout");
        fs::write(
            repository.root.join(".git/sequencer/todo"),
            "pick deadbeef\n",
        )
        .expect("write the rebase plan");
        fs::write(repository.root.join(".gitignore"), "ignored\n")
            .expect("restale the main checkout index stat cache");
        git(&repository.root, &["status", "--porcelain"]);
        let quiet = tokio::time::timeout(Duration::from_millis(500), source.next())
            .await
            .is_err();

        // A commit this worktree makes must reach it, on the ATTACHED branch
        // that is the ordinary case. That commit moves only the shared branch
        // ref, which stays unwatched, and touches no working-tree file; the
        // private `logs/HEAD` it appends to is the whole of the witness.
        git(
            &linked,
            &["commit", "--quiet", "--allow-empty", "-m", "reflog"],
        );
        let committed = tokio::time::timeout(Duration::from_secs(2), source.next()).await;
        let committed = match committed {
            Ok(event) => {
                let event = event
                    .expect("read commit event")
                    .expect("source remains open");
                has_rerun(
                    &collect_batch(event, &mut source),
                    DevStage::Migrate,
                    DevSourceState::Clean,
                )
            }
            Err(_elapsed) => false,
        };

        drop(source);
        git(
            &repository.root,
            &["worktree", "remove", "--force", linked_argument],
        );

        assert!(
            shared.is_empty(),
            "a linked worktree watched {shared:?} under the shared {}",
            shared_git_dir.display()
        );
        assert!(
            outside.is_empty(),
            "a linked worktree watched {outside:?}, outside both {} and {}",
            worktree_root.display(),
            own_git_dir.display()
        );
        assert!(
            watches_own_git_dir,
            "a linked worktree must watch its own {}",
            own_git_dir.display()
        );
        assert_eq!(
            metadata,
            vec![own_git_dir.join("HEAD"), own_git_dir.join("logs/HEAD")],
            "only `HEAD` and its reflog in the private Git directory are \
             watchable from a linked worktree; the shared refs and the private \
             index are not"
        );
        assert!(
            quiet,
            "an unrelated checkout's Git activity must not reach this loop"
        );
        assert!(
            committed,
            "a commit on an attached branch that changed no working-tree file \
             must rerun the loop"
        );
    }

    /// The reflog is the linked worktree's only witness of a commit on an
    /// attached branch, and `core.logAllRefUpdates` can turn it off. Watch
    /// mode refuses rather than watching a file Git will never write, and the
    /// refusal names the setting so an operator can act on it.
    #[tokio::test]
    async fn watch_mode_refuses_a_repository_with_the_reflog_turned_off() {
        let repository = TempRepository::new();
        repository.write_fixture();
        git(
            &repository.root,
            &["config", "core.logAllRefUpdates", "false"],
        );
        let git_source = GitSource::discover(&repository.root)
            .await
            .expect("discover source repository");

        let refusal = FilesystemInvalidationSource::new(
            [repository.package()],
            [repository.component()],
            git_source,
        )
        .await
        .err()
        .map(|error| (error.kind(), error.to_string()));

        let (kind, rendered) = refusal.expect("watch mode must refuse a disabled reflog");
        assert_eq!(kind, FilesystemInvalidationErrorKind::Git);
        assert!(
            rendered.contains("core.logAllRefUpdates"),
            "the refusal must name the setting an operator has to change: \
             {rendered}"
        );
    }

    /// The index is not commit metadata, because a plain `git status` typed
    /// in the worktree rewrites it and a commit is already seen through the
    /// ref that status does not touch.
    #[tokio::test]
    async fn a_refreshed_index_is_not_commit_metadata() {
        let repository = TempRepository::new();
        repository.write_fixture();
        let git_source = GitSource::discover(&repository.root)
            .await
            .expect("discover source repository");
        let mut source = FilesystemInvalidationSource::new(
            [repository.package()],
            [repository.component()],
            git_source,
        )
        .await
        .expect("construct filesystem invalidation source");

        let index = repository.root.join(".git/index");
        assert!(index.is_file(), "the fixture committed through an index");
        assert!(
            !source.roots.is_git_metadata(&index),
            "the index is not a watched metadata path"
        );
        assert!(
            source
                .git
                .metadata_paths()
                .iter()
                .any(|path| path.starts_with(repository.root.join(".git/refs/heads"))),
            "the current branch ref still is: {:?}",
            source.git.metadata_paths()
        );

        // Rewriting a tracked file outside every watched root with its own
        // bytes leaves the source unchanged but the cached stat stale, which
        // is what makes the next status write the index.
        fs::write(repository.root.join(".gitignore"), "ignored\n")
            .expect("restale the index stat cache");
        git(&repository.root, &["status", "--porcelain"]);
        if let Ok(event) = tokio::time::timeout(Duration::from_millis(500), source.next()).await {
            let event = event
                .expect("read status event")
                .expect("source remains open");
            assert!(
                collect_batch(event, &mut source)
                    .iter()
                    .all(|event| *event == DevInvalidation::Ignore),
                "typing `git status` must not rerun the loop"
            );
        }
    }
}
