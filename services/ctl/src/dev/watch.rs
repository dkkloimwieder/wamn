//! Git source-state and filesystem invalidation adapters for `wamn dev`.
//!
//! The adapter maps package-owned inputs and explicit component source roots
//! into engine stage identities. It deliberately does not coalesce events or
//! execute stages; [`super::run_watch`] remains the sole orchestration owner.

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
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

/// The commit-metadata paths inside this worktree: `HEAD`, its ref, and the
/// packed refs that ref can be folded into.
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
/// What a linked worktree recovers is therefore `HEAD` alone: a branch switch
/// or a commit on a DETACHED `HEAD` rewrites it and reruns the loop, but a
/// commit on an attached branch moves only the shared branch ref, so it is
/// still not seen. The index is inside the private directory too, so the rule
/// names and drops it for the reason above.
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
    let mut paths = BTreeSet::from([git_dir.join("HEAD"), packed_refs]);
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
            return Some(DevStage::Generate);
        }
        if relative.starts_with("publication/wirings") {
            return Some(DevStage::Generate);
        }
        if relative == Path::new("publication/attachments.json") {
            return Some(DevStage::Generate);
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

/// Exact native output bytes acknowledged at the package's emission boundary.
///
/// The source also remembers an observed external change after requesting Build,
/// so duplicate notifications for the same bytes do not request another run.
#[derive(Clone, Default)]
pub(super) struct GeneratedNativeOutputs {
    packages: std::sync::Arc<std::sync::Mutex<BTreeMap<PathBuf, NativeOutputFiles>>>,
}

type NativeOutputFiles = BTreeMap<PathBuf, Vec<u8>>;

impl fmt::Debug for GeneratedNativeOutputs {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GeneratedNativeOutputs")
            .finish_non_exhaustive()
    }
}

impl GeneratedNativeOutputs {
    /// Call immediately after this package's own generation succeeds.
    pub(super) fn acknowledge(&self, package: &Path) -> Result<(), FilesystemInvalidationError> {
        let files = native_output_files(package)?;
        self.packages
            .lock()
            .expect("native output snapshot mutex is not poisoned")
            .insert(package.to_owned(), files);
        Ok(())
    }

    fn observe_changes(
        &self,
        changes: &[RawChange],
    ) -> Result<BTreeSet<PathBuf>, FilesystemInvalidationError> {
        let mut packages = self
            .packages
            .lock()
            .expect("native output snapshot mutex is not poisoned");
        let mut invalidated = BTreeSet::new();
        for (package, previous) in packages.iter_mut() {
            if !changes.iter().any(|change| {
                change
                    .path
                    .as_deref()
                    .is_some_and(|path| affects_native_output(package, path))
            }) {
                continue;
            }
            let current = native_output_files(package)?;
            if current != *previous {
                invalidated.insert(package.clone());
                *previous = current;
            }
        }
        Ok(invalidated)
    }
}

fn native_output_paths(package: &Path) -> [PathBuf; 3] {
    let mut directory = package
        .file_name()
        .expect("a declared package root has a directory name")
        .to_owned();
    directory.push("-tui");
    let tui = package.join("generated").join(directory);
    [
        tui.join("Cargo.toml"),
        tui.join("src"),
        package.join("generated/client"),
    ]
}

fn affects_native_output(package: &Path, path: &Path) -> bool {
    native_output_paths(package)
        .iter()
        .any(|root| path.starts_with(root) || root.starts_with(path))
}

fn native_output_files(package: &Path) -> Result<NativeOutputFiles, FilesystemInvalidationError> {
    let mut files = BTreeMap::new();
    let mut pending = native_output_paths(package).to_vec();
    while let Some(path) = pending.pop() {
        let read_error = |source| {
            FilesystemInvalidationError::with_source(
                FilesystemInvalidationErrorKind::Read,
                &path,
                "cannot snapshot generated native input",
                source,
            )
        };
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(source) if source.kind() == io::ErrorKind::NotFound => continue,
            Err(source) => return Err(read_error(source)),
        };
        if metadata.is_dir() {
            let entries = match fs::read_dir(&path) {
                Ok(entries) => entries,
                Err(source) if source.kind() == io::ErrorKind::NotFound => continue,
                Err(source) => return Err(read_error(source)),
            };
            for entry in entries {
                pending.push(entry.map_err(read_error)?.path());
            }
        } else if metadata.is_file() {
            let bytes = match fs::read(&path) {
                Ok(bytes) => bytes,
                Err(source) if source.kind() == io::ErrorKind::NotFound => continue,
                Err(source) => return Err(read_error(source)),
            };
            files.insert(path, bytes);
        } else {
            return Err(FilesystemInvalidationError::new(
                FilesystemInvalidationErrorKind::Read,
                &path,
                "generated native input must be a regular file or directory",
            ));
        }
    }
    Ok(files)
}

#[derive(Debug)]
struct WatchRoots {
    packages: Vec<PackageRoot>,
    component_build_roots: Box<[PathBuf]>,
    native_build_roots: Box<[PathBuf]>,
    native_build_files: BTreeSet<PathBuf>,
    watch_generated_native: bool,
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
            native_build_roots: Box::new([]),
            native_build_files: BTreeSet::new(),
            watch_generated_native: false,
            git_metadata: git.metadata_paths().iter().cloned().collect(),
            excluded: BTreeSet::new(),
        })
    }

    fn replace_native_inputs(
        &mut self,
        directories: impl IntoIterator<Item = PathBuf>,
        files: impl IntoIterator<Item = PathBuf>,
        repository: &Path,
    ) -> Result<(), FilesystemInvalidationError> {
        let directories = directories
            .into_iter()
            .map(|path| {
                let directory = path.canonicalize().map_err(|source| {
                    FilesystemInvalidationError::with_source(
                        FilesystemInvalidationErrorKind::ComponentRoot,
                        &path,
                        "cannot resolve native build root",
                        source,
                    )
                })?;
                if !directory.starts_with(repository) || !directory.is_dir() {
                    return Err(FilesystemInvalidationError::new(
                        FilesystemInvalidationErrorKind::ComponentRoot,
                        &path,
                        "native build root is not a directory inside the originating Git worktree",
                    ));
                }
                Ok(directory)
            })
            .collect::<Result<BTreeSet<_>, _>>()?;
        let files = files.into_iter().map(|path| {
            if !path.starts_with(repository)
                || path.components().any(|part| part == Component::ParentDir)
                || path.is_dir()
            {
                return Err(FilesystemInvalidationError::new(
                    FilesystemInvalidationErrorKind::ComponentRoot,
                    &path,
                    "native build file is not an exact file inside the originating Git worktree",
                ));
            }
            let existing = path.ancestors().find(|ancestor| ancestor.exists())
                .expect("the originating repository exists");
            let resolved = existing.canonicalize().map_err(|source| {
                FilesystemInvalidationError::with_source(
                    FilesystemInvalidationErrorKind::ComponentRoot,
                    &path,
                    "cannot resolve native build file parent",
                    source,
                )
            })?;
            if !resolved.starts_with(repository) {
                return Err(FilesystemInvalidationError::new(
                    FilesystemInvalidationErrorKind::ComponentRoot,
                    &path,
                    "native build file resolves outside the originating Git worktree",
                ));
            }
            Ok(path)
        }).collect::<Result<BTreeSet<_>, _>>()?;
        self.native_build_roots = directories.into_iter().collect();
        self.native_build_files = files;
        Ok(())
    }

    fn native_file_or_parent(&self, path: &Path) -> bool {
        self.native_build_files
            .iter()
            .any(|file| file == path || file.starts_with(path))
    }

    fn owns_recursive_path(&self, path: &Path) -> bool {
        self.watched_roots().any(|root| path.starts_with(root))
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
            return (self.watch_generated_native
                && self
                    .packages
                    .iter()
                    .any(|package| affects_native_output(&package.root, path)))
            .then_some(DevStage::Build);
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
        let native_stage = (self.native_file_or_parent(path)
            || self
                .native_build_roots
                .iter()
                .any(|root| path.starts_with(root)))
        .then_some(DevStage::Build);
        package_stage
            .into_iter()
            .chain(component_stage)
            .chain(native_stage)
            .min_by_key(|stage| stage.position())
    }

    fn is_changed_input(&self, path: &Path, changed_native: &BTreeSet<PathBuf>) -> bool {
        self.stage(path).is_some()
            && (!self
                .packages
                .iter()
                .any(|package| package.owns_generated(path))
                || changed_native
                    .iter()
                    .any(|package| affects_native_output(package, path)))
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
            .chain(self.native_build_roots.iter().map(PathBuf::as_path))
    }
}

#[derive(Debug)]
struct RawChange {
    path: Option<PathBuf>,
    events: ReadFlags,
}

/// Cargo can create a staging directory and rename it into ignored output
/// before the watcher drains events. Its absent, never-watched origin cannot
/// remove existing source; a surviving destination has its own notification.
fn vanished_unwatched_directory(
    change: &RawChange,
    watched_directories: &HashMap<i32, PathBuf>,
) -> bool {
    change.events.contains(ReadFlags::ISDIR)
        && change
            .events
            .intersects(ReadFlags::CREATE | ReadFlags::MOVED_FROM | ReadFlags::DELETE)
        && change.path.as_deref().is_some_and(|path| {
            !path.exists()
                && !watched_directories
                    .values()
                    .any(|directory| directory.starts_with(path))
        })
}

/// Linux filesystem invalidation source over explicit package and build roots.
pub struct FilesystemInvalidationSource {
    inotify: AsyncFd<OwnedFd>,
    watched_directories: HashMap<i32, PathBuf>,
    roots: WatchRoots,
    git: GitSource,
    pending: VecDeque<DevInvalidation>,
    generated_native_outputs: Option<GeneratedNativeOutputs>,
}

impl fmt::Debug for FilesystemInvalidationSource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FilesystemInvalidationSource")
            .field("watched_directories", &self.watched_directories.len())
            .field("roots", &self.roots)
            .field("git", &self.git)
            .field("pending", &self.pending.len())
            .field("generated_native_outputs", &self.generated_native_outputs)
            .finish()
    }
}

impl FilesystemInvalidationSource {
    /// Register recursive event watches for exact roots supplied by the caller.
    pub async fn new(
        package_roots: impl IntoIterator<Item = PathBuf>,
        component_build_roots: impl IntoIterator<Item = PathBuf>,
        git: GitSource,
    ) -> Result<Self, FilesystemInvalidationError> {
        Self::with_native_inputs(
            package_roots,
            component_build_roots,
            std::iter::empty(),
            std::iter::empty(),
            git,
        )
        .await
    }

    /// Watch native source trees and exact files without traversing their parents.
    pub async fn with_native_inputs(
        package_roots: impl IntoIterator<Item = PathBuf>,
        component_build_roots: impl IntoIterator<Item = PathBuf>,
        native_directories: impl IntoIterator<Item = PathBuf>,
        native_files: impl IntoIterator<Item = PathBuf>,
        git: GitSource,
    ) -> Result<Self, FilesystemInvalidationError> {
        let mut roots = WatchRoots::new(package_roots, component_build_roots, &git)?;
        roots.replace_native_inputs(native_directories, native_files, git.repository_root())?;
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
            generated_native_outputs: None,
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
        source.add_native_file_parents()?;
        Ok(source)
    }

    /// Share emission acknowledgments with the runner before the first run.
    pub(super) fn watch_generated_native_outputs(
        &mut self,
    ) -> Result<GeneratedNativeOutputs, FilesystemInvalidationError> {
        let outputs = GeneratedNativeOutputs::default();
        for package in &self.roots.packages {
            outputs.acknowledge(&package.root)?;
        }
        self.roots.watch_generated_native = true;
        self.generated_native_outputs = Some(outputs.clone());
        Ok(outputs)
    }

    /// Refresh native dependency ownership after a completed generation and build.
    pub async fn replace_native_inputs(
        &mut self,
        directories: impl IntoIterator<Item = PathBuf>,
        files: impl IntoIterator<Item = PathBuf>,
    ) -> Result<(), FilesystemInvalidationError> {
        self.roots
            .replace_native_inputs(directories, files, self.git.repository_root())?;
        let directories = self.roots.native_build_roots.to_vec();
        for directory in directories {
            self.add_tree(&directory).await?;
        }
        self.add_native_file_parents()
    }

    fn add_native_file_parents(&mut self) -> Result<(), FilesystemInvalidationError> {
        let files = self
            .roots
            .native_build_files
            .iter()
            .cloned()
            .collect::<Vec<_>>();
        for parent in metadata_watch_directories(&files) {
            self.add_directory(&parent)?;
        }
        Ok(())
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

        // Preserve membership before a moved inode is registered at its new
        // path: inotify reuses its descriptor and replaces the old path entry.
        let vanished_directories = changes
            .iter()
            .enumerate()
            .filter_map(|(index, change)| {
                vanished_unwatched_directory(change, &self.watched_directories).then_some(index)
            })
            .collect::<BTreeSet<_>>();

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
                if self.roots.owns_recursive_path(path) || self.roots.is_git_metadata(path) {
                    self.add_tree(path).await?;
                }
                if self.roots.native_file_or_parent(path) {
                    self.add_native_file_parents()?;
                }
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

        let changed_native = self
            .generated_native_outputs
            .as_ref()
            .map(|outputs| outputs.observe_changes(&changes))
            .transpose()?
            .unwrap_or_default();
        let needs_source_state = changes.iter().enumerate().any(|(index, change)| {
            change.events.contains(ReadFlags::QUEUE_OVERFLOW)
                || (!vanished_directories.contains(&index)
                    && change.path.as_deref().is_some_and(|path| {
                        self.roots.is_changed_input(path, &changed_native)
                            || self.roots.is_git_metadata(path)
                    }))
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

        for (index, change) in changes.into_iter().enumerate() {
            let invalidation = if change.events.contains(ReadFlags::QUEUE_OVERFLOW) {
                tracing::debug!(
                    event_mask = change.events.bits(),
                    "development filesystem queue overflow requires rerun"
                );
                DevInvalidation::Rerun {
                    from: DevStage::Migrate,
                    source_state,
                }
            } else if vanished_directories.contains(&index) {
                DevInvalidation::Ignore
            } else if let Some(path) = change.path {
                // Path classification decides RELEVANCE, not the starting
                // stage. Every relevant change reruns the whole pipeline,
                // because the target database is recreated per run and a
                // suffix that started after Apply would run against an empty
                // one. What a run does not have to redo is decided by each
                // stage's input digest, not by which file was touched.
                if self.roots.is_git_metadata(&path)
                    || self.roots.is_changed_input(&path, &changed_native)
                {
                    tracing::debug!(
                        path = %path.display(),
                        event_mask = change.events.bits(),
                        stage_owner = ?self.roots.stage(&path),
                        git_metadata = self.roots.is_git_metadata(&path),
                        generated_native_output = changed_native
                            .iter()
                            .any(|package| affects_native_output(package, &path)),
                        "development filesystem input requires rerun"
                    );
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
            Some(DevStage::Generate)
        );
        assert_eq!(
            package.stage(&root.join("publication/wirings/receiving.json")),
            Some(DevStage::Generate)
        );
        assert_eq!(
            package.stage(&root.join("publication/attachments.json")),
            Some(DevStage::Generate)
        );
        assert_eq!(package.stage(&root.join("README.md")), None);
    }

    #[tokio::test]
    async fn exact_native_files_do_not_recursively_watch_the_repository() {
        let repository = TempRepository::new();
        repository.write_fixture();
        let native = repository.root.join("native");
        fs::create_dir_all(native.join("src")).expect("create native source root");
        let unrelated = repository.root.join("unrelated");
        fs::create_dir_all(unrelated.join("nested")).expect("create unrelated source root");
        let manifest = repository.root.join("Cargo.toml");
        fs::write(&manifest, "[workspace]\n").expect("write root manifest");
        let git = GitSource::discover(&repository.root)
            .await
            .expect("discover source repository");
        let mut source = FilesystemInvalidationSource::with_native_inputs(
            [repository.package()],
            [repository.component()],
            [native.clone()],
            [manifest.clone()],
            git,
        )
        .await
        .expect("construct exact native file watch");
        assert!(
            source
                .watched_directories
                .values()
                .any(|path| path == &repository.root)
        );
        assert!(
            !source
                .watched_directories
                .values()
                .any(|path| path.starts_with(&unrelated))
        );
        assert_eq!(source.roots.stage(&manifest), Some(DevStage::Build));
        assert_eq!(
            source.roots.stage(&native.join("src/lib.rs")),
            Some(DevStage::Build)
        );
        assert_eq!(source.roots.stage(&repository.root.join("README.md")), None);
        assert_eq!(
            source.roots.stage(
                &repository
                    .package()
                    .join("generated/receiving-tui/src/main.rs")
            ),
            None
        );

        let added = repository.root.join("new-unrelated");
        fs::create_dir_all(added.join("nested"))
            .expect("create unrelated directories after startup");
        let first = tokio::time::timeout(Duration::from_secs(2), source.next())
            .await
            .expect("directory event arrived")
            .expect("read directory event")
            .expect("watch remains open");
        assert!(
            collect_batch(first, &mut source)
                .iter()
                .all(|event| *event == DevInvalidation::Ignore)
        );
        assert!(
            !source
                .watched_directories
                .values()
                .any(|path| path.starts_with(&added))
        );

        fs::write(&manifest, "[workspace]\nresolver = \"2\"\n").expect("edit root manifest");
        let first = tokio::time::timeout(Duration::from_secs(2), source.next())
            .await
            .expect("manifest event arrived")
            .expect("read manifest event")
            .expect("watch remains open");
        assert!(has_rerun(
            &collect_batch(first, &mut source),
            DevStage::Migrate,
            DevSourceState::Dirty
        ));
    }

    #[tokio::test]
    async fn an_absent_cargo_configuration_parent_is_watched_when_created() {
        let repository = TempRepository::new();
        repository.write_fixture();
        let config = repository.root.join(".cargo/config.toml");
        let git = GitSource::discover(&repository.root)
            .await
            .expect("discover source repository");
        let mut source = FilesystemInvalidationSource::with_native_inputs(
            [repository.package()],
            [repository.component()],
            [],
            [config.clone()],
            git,
        )
        .await
        .expect("watch an absent exact configuration file");
        fs::create_dir(config.parent().expect("configuration parent"))
            .expect("create Cargo directory");
        fs::write(&config, "[build]\njobs = 1\n")
            .expect("create Cargo configuration before reading events");
        let first = tokio::time::timeout(Duration::from_secs(2), source.next())
            .await
            .expect("Cargo directory event arrived")
            .expect("read directory event")
            .expect("watch remains open");
        assert!(has_rerun(
            &collect_batch(first, &mut source),
            DevStage::Migrate,
            DevSourceState::Dirty
        ));
        assert!(
            source
                .watched_directories
                .values()
                .any(|path| Some(path.as_path()) == config.parent())
        );
        fs::write(&config, "[build]\njobs = 2\n").expect("edit Cargo configuration");
        let first = tokio::time::timeout(Duration::from_secs(2), source.next())
            .await
            .expect("configuration event arrived")
            .expect("read configuration event")
            .expect("watch remains open");
        assert!(has_rerun(
            &collect_batch(first, &mut source),
            DevStage::Migrate,
            DevSourceState::Dirty
        ));
        assert_eq!(
            source
                .roots
                .stage(&repository.root.join(".cargo/unrelated")),
            None
        );
    }

    #[tokio::test]
    async fn refreshed_native_dependencies_replace_ownership_and_retain_component_roots() {
        let repository = TempRepository::new();
        repository.write_fixture();
        let first = repository.root.join("first-native");
        let second = repository.root.join("second-native");
        fs::create_dir(&first).expect("create first native root");
        fs::create_dir(&second).expect("create second native root");
        let git = GitSource::discover(&repository.root)
            .await
            .expect("discover source repository");
        let mut source = FilesystemInvalidationSource::with_native_inputs(
            [repository.package()],
            [repository.component()],
            [first.clone()],
            [repository.root.join("Cargo.toml")],
            git,
        )
        .await
        .expect("construct initial native dependency watch");
        source
            .replace_native_inputs([second.clone()], [repository.root.join("Cargo.lock")])
            .await
            .expect("refresh the native dependency graph");
        assert_eq!(source.roots.stage(&first.join("lib.rs")), None);
        assert_eq!(
            source.roots.stage(&second.join("lib.rs")),
            Some(DevStage::Build)
        );
        assert_eq!(
            source
                .roots
                .stage(&repository.component().join("src/lib.rs")),
            Some(DevStage::Build)
        );
        assert_eq!(
            source.roots.stage(&repository.root.join("Cargo.toml")),
            None
        );
        assert_eq!(
            source.roots.stage(&repository.root.join("Cargo.lock")),
            Some(DevStage::Build)
        );
        assert!(
            source
                .replace_native_inputs([], [repository.root.join("../outside.toml")])
                .await
                .is_err()
        );
        assert_eq!(
            source.roots.stage(&second.join("lib.rs")),
            Some(DevStage::Build),
            "a refused refresh retains the prior ownership"
        );
    }

    async fn native_output_source(
        repository: &TempRepository,
    ) -> (
        FilesystemInvalidationSource,
        GeneratedNativeOutputs,
        [PathBuf; 3],
    ) {
        let paths = [
            repository
                .package()
                .join("generated/package-tui/Cargo.toml"),
            repository
                .package()
                .join("generated/package-tui/src/lib.rs"),
            repository.package().join("generated/client/location.rs"),
        ];
        for path in &paths {
            fs::create_dir_all(path.parent().expect("native output parent"))
                .expect("create native output directory");
            fs::write(path, "initial output").expect("write initial native output");
        }
        // Generated native sources are tracked in real packages. Keep their
        // deletion dirty too, including the last file removed by the test.
        git(&repository.root, &["add", "package/generated"]);
        git(
            &repository.root,
            &["commit", "--quiet", "-m", "generated native fixture"],
        );
        let git = GitSource::discover(&repository.root)
            .await
            .expect("discover native output fixture");
        let mut source = FilesystemInvalidationSource::new(
            [repository.package()],
            [repository.component()],
            git,
        )
        .await
        .expect("watch native output fixture");
        let outputs = source
            .watch_generated_native_outputs()
            .expect("acknowledge initial native outputs");
        (source, outputs, paths)
    }

    async fn native_output_batch(
        source: &mut FilesystemInvalidationSource,
    ) -> Vec<DevInvalidation> {
        let first = tokio::time::timeout(Duration::from_secs(2), source.next())
            .await
            .expect("native output event arrives")
            .expect("read native output event")
            .expect("native output source stays open");
        collect_batch(first, source)
    }

    #[tokio::test]
    async fn external_native_emission_edits_and_deletions_require_build() {
        let repository = TempRepository::new();
        repository.write_fixture();
        let (mut source, _outputs, paths) = native_output_source(&repository).await;
        for path in &paths {
            assert_eq!(source.roots.stage(path), Some(DevStage::Build));
            fs::write(path, "external output").expect("edit native output externally");
            assert!(has_rerun(
                &native_output_batch(&mut source).await,
                DevStage::Migrate,
                DevSourceState::Dirty,
            ));
            fs::remove_file(path).expect("delete native output externally");
            assert!(has_rerun(
                &native_output_batch(&mut source).await,
                DevStage::Migrate,
                DevSourceState::Dirty,
            ));
        }
        let application = repository.package().join("generated/wamn.rs");
        fs::write(&application, "application generation stays ignored")
            .expect("write unrelated generated application output");
        assert_eq!(source.roots.stage(&application), None);
        assert!(
            native_output_batch(&mut source)
                .await
                .iter()
                .all(|event| *event == DevInvalidation::Ignore)
        );
    }

    #[tokio::test]
    async fn acknowledged_own_emission_and_identical_external_bytes_do_not_rerun() {
        let repository = TempRepository::new();
        repository.write_fixture();
        let (mut source, outputs, paths) = native_output_source(&repository).await;
        for path in &paths {
            fs::write(path, "own emission").expect("write the loop's own output");
        }
        outputs
            .acknowledge(&repository.package())
            .expect("acknowledge immediately after emission");
        // The own CLOSE_WRITE events are still in the kernel queue here.
        assert!(
            native_output_batch(&mut source)
                .await
                .iter()
                .all(|event| *event == DevInvalidation::Ignore)
        );
        fs::write(&paths[1], "own emission").expect("rewrite identical source bytes");
        assert!(
            native_output_batch(&mut source)
                .await
                .iter()
                .all(|event| *event == DevInvalidation::Ignore)
        );
    }

    #[tokio::test]
    async fn external_changes_after_emission_survive_queued_events_and_metadata_refresh() {
        let repository = TempRepository::new();
        repository.write_fixture();
        let (mut source, outputs, paths) = native_output_source(&repository).await;
        for delete in [false, true] {
            for path in &paths {
                fs::write(path, "own emission").expect("write the loop's next emission");
            }
            outputs
                .acknowledge(&repository.package())
                .expect("acknowledge before later stages run");
            if delete {
                fs::remove_file(&paths[1]).expect("delete source during a later stage");
            } else {
                fs::write(&paths[2], "external edit during Build")
                    .expect("edit bindings during a later stage");
            }
            // NativeInvalidations refreshes these after a run. That refresh
            // must not accept external bytes as if Generate had emitted them.
            source
                .replace_native_inputs(
                    [repository.component()],
                    [repository.root.join("Cargo.toml")],
                )
                .await
                .expect("refresh native dependency ownership");
            assert!(has_rerun(
                &native_output_batch(&mut source).await,
                DevStage::Migrate,
                DevSourceState::Dirty,
            ));
        }
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

    #[tokio::test]
    async fn first_build_outputs_in_overlapping_component_roots_do_not_rerun() {
        let repository = TempRepository::new();
        repository.write_fixture();
        let components = repository.root.join("components");
        let no_std = components.join("no-std");
        fs::create_dir_all(no_std.join("guest/src")).expect("create nested component workspace");
        fs::write(
            repository.root.join(".gitignore"),
            "ignored\n/components/target\n/components/no-std/target\n",
        )
        .expect("ignore both declared component output directories");
        let outputs = [components.join("target"), no_std.join("target")];
        let native_files = [
            repository.root.join("Cargo.toml"),
            repository.root.join("Cargo.lock"),
        ];
        let git = GitSource::discover(&repository.root)
            .await
            .expect("discover first-build fixture");
        let mut source = FilesystemInvalidationSource::with_native_inputs(
            [repository.package()],
            [components, no_std],
            [repository.component()],
            native_files.clone(),
            git,
        )
        .await
        .expect("watch overlapping component and native roots");
        let generated = source
            .watch_generated_native_outputs()
            .expect("enable native emission acknowledgments");
        for output in &outputs {
            assert!(!output.exists());
            assert!(
                !source.roots.is_excluded(output),
                "first-ever output has no prior exclusion"
            );
        }

        let emitted = repository
            .package()
            .join("generated/package-tui/src/lib.rs");
        fs::create_dir_all(emitted.parent().expect("generated source parent"))
            .expect("create first generated native source directory");
        fs::write(emitted, "own first emission").expect("emit native source before Build");
        generated
            .acknowledge(&repository.package())
            .expect("acknowledge this package immediately after Generate");
        for output in &outputs {
            // Cargo first creates a temporary sibling, then renames it into
            // target. The origin no longer exists when its events are read.
            let staging = output.with_file_name("targetbpz54B");
            fs::create_dir_all(staging.join("wasm32-wasip2/release"))
                .expect("the first Build creates its staging tree");
            fs::write(
                staging.join("wasm32-wasip2/release/guest.wasm"),
                "built artifact",
            )
            .expect("write the first component artifact");
            fs::write(staging.join(".cargo-lock"), "").expect("write build lock");
            fs::rename(staging, output).expect("publish the first ignored target directory");
        }
        // As in NativeInvalidations, refresh after the run while Generate and
        // first-build CREATE events are still queued in the kernel.
        source
            .replace_native_inputs([repository.component()], native_files)
            .await
            .expect("refresh native dependencies before reading first-build events");
        assert!(
            native_output_batch(&mut source)
                .await
                .iter()
                .all(|event| *event == DevInvalidation::Ignore),
            "own emission and first-ever ignored outputs must leave the activation alive"
        );
        for output in &outputs {
            assert!(source.roots.is_excluded(output));
            assert!(
                !source
                    .watched_directories
                    .values()
                    .any(|directory| directory.starts_with(output)),
                "new ignored output trees must not acquire recursive watches"
            );
        }
        assert!(
            tokio::time::timeout(Duration::from_millis(500), source.next())
                .await
                .is_err(),
            "registration must not create a later feedback event"
        );
    }

    #[tokio::test]
    async fn source_removal_still_reruns_when_the_destination_is_ignored_output() {
        let repository = TempRepository::new();
        repository.write_fixture();
        let authored = repository.component().join("target-authored");
        fs::create_dir(&authored).expect("create authored source directory");
        fs::write(authored.join("lib.rs"), "pub fn authored() {}")
            .expect("write authored source before watching");
        let archived_source = repository.component().join("source-to-archive");
        fs::create_dir(&archived_source).expect("create source later moved into generated output");
        fs::write(archived_source.join("lib.rs"), "pub fn archived() {}")
            .expect("write archived source before watching");
        git(&repository.root, &["add", "."]);
        git(
            &repository.root,
            &["commit", "--quiet", "-m", "authored source"],
        );
        let git = GitSource::discover(&repository.root)
            .await
            .expect("discover source-removal fixture");
        let mut source = FilesystemInvalidationSource::new(
            [repository.package()],
            [repository.component()],
            git,
        )
        .await
        .expect("watch authored source before its move");
        fs::rename(&authored, repository.component().join("ignored"))
            .expect("move watched source into ignored output");
        assert!(
            has_rerun(
                &native_output_batch(&mut source).await,
                DevStage::Migrate,
                DevSourceState::Dirty
            ),
            "a watched source moved into output still disappeared from the build"
        );

        fs::rename(
            &archived_source,
            repository.package().join("generated/archive"),
        )
        .expect("move watched source into watched but ignored generated output");
        assert!(
            has_rerun(
                &native_output_batch(&mut source).await,
                DevStage::Migrate,
                DevSourceState::Dirty
            ),
            "registering the moved inode must not erase its previous source ownership"
        );

        fs::remove_file(repository.component().join("src/lib.rs"))
            .expect("delete another tracked source");
        assert!(
            has_rerun(
                &native_output_batch(&mut source).await,
                DevStage::Migrate,
                DevSourceState::Dirty
            ),
            "ordinary source deletion must still invalidate"
        );

        let new_source = repository.component().join("target-new-source");
        fs::create_dir(&new_source).expect("create a real target-prefixed source directory");
        fs::write(new_source.join("lib.rs"), "pub fn new_source() {}")
            .expect("write new source with no move into ignored output");
        assert!(
            has_rerun(
                &native_output_batch(&mut source).await,
                DevStage::Migrate,
                DevSourceState::Dirty
            ),
            "a target prefix alone must never hide authored source"
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
    /// the private one cannot, and is what brings `HEAD` back (wamn-10yt.71).
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

        // A commit this worktree makes must reach it. Detaching is what puts
        // the commit in the private directory: on an attached branch the
        // commit moves only the SHARED branch ref, which is unwatched by
        // design, so `HEAD` is the whole of what a linked worktree recovers.
        git(&linked, &["checkout", "--detach", "--quiet"]);
        while tokio::time::timeout(Duration::from_millis(500), source.next())
            .await
            .is_ok()
        {}
        git(
            &linked,
            &["commit", "--quiet", "--allow-empty", "-m", "head"],
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
            vec![own_git_dir.join("HEAD")],
            "only `HEAD` in the private Git directory is watchable from a \
             linked worktree; the shared refs and the private index are not"
        );
        assert!(
            quiet,
            "an unrelated checkout's Git activity must not reach this loop"
        );
        assert!(
            committed,
            "a commit that changed no working-tree file must rerun the loop"
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
