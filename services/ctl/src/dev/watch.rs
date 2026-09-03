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
use std::process::Output;

use rustix::fs::inotify::{self, CreateFlags, ReadFlags, WatchFlags};
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

async fn discover_metadata_paths(
    repository_root: &Path,
    git_dir: &Path,
) -> Result<Vec<PathBuf>, GitSourceError> {
    let index = git_path_output(
        repository_root,
        &["rev-parse", "--path-format=absolute", "--git-path", "index"],
        GitSourceErrorKind::Discover,
        "discover Git index",
    )
    .await?;
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
    let mut paths = BTreeSet::from([git_dir.join("HEAD"), index, packed_refs]);
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
        })
    }

    fn stage(&self, path: &Path) -> Option<DevStage> {
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
    pub fn new(
        package_roots: impl IntoIterator<Item = PathBuf>,
        component_build_roots: impl IntoIterator<Item = PathBuf>,
        git: GitSource,
    ) -> Result<Self, FilesystemInvalidationError> {
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
            source.add_tree(&root)?;
        }
        let metadata_parents = metadata_watch_directories(source.git.metadata_paths());
        for parent in metadata_parents {
            source.add_directory(&parent)?;
        }
        Ok(source)
    }

    fn add_tree(&mut self, root: &Path) -> Result<(), FilesystemInvalidationError> {
        let mut pending = vec![root.to_owned()];
        while let Some(directory) = pending.pop() {
            self.add_directory(&directory)?;
            let entries = fs::read_dir(&directory).map_err(|source| {
                FilesystemInvalidationError::with_source(
                    FilesystemInvalidationErrorKind::Watch,
                    &directory,
                    "cannot enumerate watched directory",
                    source,
                )
            })?;
            for entry in entries {
                let entry = entry.map_err(|source| {
                    FilesystemInvalidationError::with_source(
                        FilesystemInvalidationErrorKind::Watch,
                        &directory,
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
                    pending.push(entry.path());
                }
            }
        }
        Ok(())
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
                self.add_tree(path)?;
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
                if self.roots.is_git_metadata(&path) {
                    DevInvalidation::Rerun {
                        from: DevStage::Publish,
                        source_state,
                    }
                } else if let Some(from) = self.roots.stage(&path) {
                    DevInvalidation::Rerun { from, source_state }
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
            DevStage::Generate,
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
            DevStage::Build,
            DevSourceState::Dirty
        ));
    }

    #[tokio::test]
    async fn clean_commit_metadata_resumes_at_publish_without_another_package_edit() {
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
        assert!(has_rerun(&dirty, DevStage::Generate, DevSourceState::Dirty));

        git(&repository.root, &["add", "."]);
        git(&repository.root, &["commit", "--quiet", "-m", "save edit"]);
        let committed = tokio::time::timeout(Duration::from_secs(2), source.next())
            .await
            .expect("commit metadata event arrived")
            .expect("read commit metadata event")
            .expect("source remains open");
        let committed = collect_batch(committed, &mut source);
        assert!(has_rerun(
            &committed,
            DevStage::Publish,
            DevSourceState::Clean
        ));
    }
}
