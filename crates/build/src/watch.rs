//! Git source-state and filesystem invalidation adapters for `wamn dev`.
//!
//! The adapter decides whether a change touches package-owned inputs or explicit
//! component source roots. It deliberately does not coalesce events or execute
//! deployment work. [`crate::dev::run`] owns each development pass.

pub(super) mod plan;

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
use std::process::Stdio;

use super::git_source;
use rustix::fs::inotify::{self, CreateFlags, ReadFlags, WatchFlags};
use tokio::io::AsyncWriteExt as _;
use tokio::io::unix::AsyncFd;
use tokio::process::Command;
use wamn_codegen::is_input_path;

pub use super::git_source::{
    GitSourceError, GitSourceErrorType, GitSourceSnapshot, GitSourceState,
};

use crate::dev::{DevInvalidation, WatchInputs};

const INOTIFY_BUFFER_BYTES: usize = 64 * 1024;

/// Production Git adapter pinned to one originating repository.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GitSource {
    repository_root: PathBuf,
}

impl GitSource {
    /// Discover the repository worktree root from `path`.
    pub async fn discover(path: impl AsRef<Path>) -> Result<Self, GitSourceError> {
        Ok(Self {
            repository_root: git_source::discover_repository_root(path.as_ref()).await?,
        })
    }

    /// Stable root of the originating worktree.
    pub fn repository_root(&self) -> &Path {
        &self.repository_root
    }

    /// Read `HEAD` and whole-worktree cleanliness from the same repository.
    pub async fn snapshot(&self) -> Result<GitSourceSnapshot, GitSourceError> {
        git_source::read_status(&self.repository_root).await
    }
}

/// Ask the originating worktree which of `candidates` its ignore rules cover.
///
/// `check-ignore` reports a tracked path as not ignored, so this answers the
/// exact question the watcher has: is this directory the repository's own
/// output rather than authored source. The Generate input digest asks the same
/// question of each package directory, so one rule bounds both.
pub(super) async fn git_ignored(
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
            GitSourceError::io(GitSourceErrorType::Inspect, OPERATION, repository, source)
        })?;
    let mut stdin = child.stdin.take().ok_or_else(|| {
        GitSourceError::output(
            GitSourceErrorType::Inspect,
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
        GitSourceError::io(GitSourceErrorType::Inspect, OPERATION, repository, source)
    })?;
    let output = output.map_err(|source| {
        GitSourceError::io(GitSourceErrorType::Inspect, OPERATION, repository, source)
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
            GitSourceErrorType::Inspect,
            OPERATION,
            repository,
            &output,
        )),
    }
}

/// Stable category of a filesystem invalidation failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FilesystemInvalidationErrorType {
    PackageRoot,
    ComponentRoot,
    Watch,
    Read,
    Git,
}

/// Failure to configure or read the production filesystem invalidation source.
#[derive(Debug)]
pub struct FilesystemInvalidationError {
    type_: FilesystemInvalidationErrorType,
    path: PathBuf,
    detail: Box<str>,
    source: Option<Box<dyn Error + Send + Sync>>,
}

impl FilesystemInvalidationError {
    fn new(
        type_: FilesystemInvalidationErrorType,
        path: impl Into<PathBuf>,
        detail: impl Into<Box<str>>,
    ) -> Self {
        Self {
            type_,
            path: path.into(),
            detail: detail.into(),
            source: None,
        }
    }

    fn with_source(
        type_: FilesystemInvalidationErrorType,
        path: impl Into<PathBuf>,
        detail: impl Into<Box<str>>,
        source: impl Error + Send + Sync + 'static,
    ) -> Self {
        Self {
            type_,
            path: path.into(),
            detail: detail.into(),
            source: Some(Box::new(source)),
        }
    }

    /// Stable error category.
    pub const fn error_type(&self) -> FilesystemInvalidationErrorType {
        self.type_
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
}

impl PackageRoot {
    fn read(root: &Path) -> Result<Self, FilesystemInvalidationError> {
        let root = root.canonicalize().map_err(|source| {
            FilesystemInvalidationError::with_source(
                FilesystemInvalidationErrorType::PackageRoot,
                root,
                "cannot resolve package root",
                source,
            )
        })?;
        Ok(Self { root })
    }
}

#[derive(Debug)]
struct WatchRoots {
    packages: Vec<PackageRoot>,
    component_build_roots: Box<[PathBuf]>,
    configuration_files: BTreeSet<PathBuf>,
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
                    FilesystemInvalidationErrorType::PackageRoot,
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
                        FilesystemInvalidationErrorType::ComponentRoot,
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
                    FilesystemInvalidationErrorType::ComponentRoot,
                    root,
                    "component build root is outside the originating Git worktree",
                ));
            }
        }
        Ok(Self {
            packages,
            component_build_roots: component_build_roots.into_boxed_slice(),
            configuration_files: BTreeSet::new(),
            excluded: BTreeSet::new(),
        })
    }

    /// Record one directory the ignore rules put outside the watch.
    fn exclude(&mut self, directory: PathBuf) {
        self.excluded.insert(directory);
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
            .finish_non_exhaustive()
    }
}

impl FilesystemInvalidationSource {
    /// Register recursive event watches for exact roots supplied by the caller.
    pub async fn new(
        package_roots: impl IntoIterator<Item = PathBuf>,
        component_build_roots: impl IntoIterator<Item = PathBuf>,
        git: GitSource,
    ) -> Result<Self, FilesystemInvalidationError> {
        let roots = WatchRoots::new(package_roots, component_build_roots, &git)?;
        let descriptor =
            inotify::init(CreateFlags::CLOEXEC | CreateFlags::NONBLOCK).map_err(|source| {
                FilesystemInvalidationError::with_source(
                    FilesystemInvalidationErrorType::Watch,
                    git.repository_root(),
                    "cannot initialize filesystem notification",
                    errno_to_io(source),
                )
            })?;
        // SAFETY: OwnedFd keeps the descriptor open and unchanged until AsyncFd drops it.
        #[expect(
            unsafe_code,
            reason = "Tokio requires an explicit descriptor lifetime guarantee"
        )]
        let inotify = unsafe { AsyncFd::register(descriptor) }.map_err(|source| {
            FilesystemInvalidationError::with_source(
                FilesystemInvalidationErrorType::Watch,
                git.repository_root(),
                "cannot register filesystem notification with the async runtime",
                io::Error::from(source),
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
        Ok(source)
    }

    /// Replace the build closure while retaining queued invalidations and exact configuration files.
    pub async fn replace_roots(
        &mut self,
        packages: Vec<PathBuf>,
        components: Vec<PathBuf>,
    ) -> Result<(), FilesystemInvalidationError> {
        let updated = WatchRoots::new(
            packages.iter().cloned(),
            components.iter().cloned(),
            &self.git,
        )?;
        if self
            .roots
            .packages
            .iter()
            .map(|root| &root.root)
            .eq(updated.packages.iter().map(|root| &root.root))
            && self.roots.component_build_roots == updated.component_build_roots
        {
            return Ok(());
        }
        let mut updated = Self::new(packages, components, self.git.clone()).await?;
        updated.replace_configuration_files(self.roots.configuration_files.iter().cloned())?;
        while let Some(event) = self.try_next() {
            updated.pending.push_back(event);
        }
        *self = updated;
        Ok(())
    }

    /// Watch explicitly configured local files, including files outside Git.
    pub fn replace_configuration_files(
        &mut self,
        files: impl IntoIterator<Item = PathBuf>,
    ) -> Result<(), FilesystemInvalidationError> {
        let files = files
            .into_iter()
            .map(|path| {
                if !path.is_absolute()
                    || path.components().any(|part| part == Component::ParentDir)
                    || path.is_dir()
                {
                    return Err(FilesystemInvalidationError::new(
                        FilesystemInvalidationErrorType::ComponentRoot,
                        &path,
                        "local configuration input must name an exact absolute file",
                    ));
                }
                Ok(path)
            })
            .collect::<Result<BTreeSet<_>, _>>()?;
        self.roots.configuration_files = files;
        self.add_exact_file_parents()
    }

    fn add_exact_file_parents(&mut self) -> Result<(), FilesystemInvalidationError> {
        let files = self
            .roots
            .configuration_files
            .iter()
            .cloned()
            .collect::<Vec<_>>();
        for parent in metadata_watch_directories(&files) {
            self.add_directory(&parent)?;
        }
        Ok(())
    }

    /// Register source directories that the build owns and Git does not ignore.
    ///
    /// Generated output must remain unwatched, including output below an
    /// overlapping component root, to prevent rebuilds from their own writes
    /// (wamn-10yt.55).
    async fn add_tree(&mut self, root: &Path) -> Result<(), FilesystemInvalidationError> {
        let mut pending = self.retain_watchable(vec![root.to_owned()]).await?;
        while let Some(directory) = pending.pop() {
            self.add_directory(&directory)?;
            let children = read_subdirectories(&directory)?;
            pending.extend(self.retain_watchable(children).await?);
        }
        Ok(())
    }

    /// Drop generated output and remember ignored directories as unwatched.
    async fn retain_watchable(
        &mut self,
        candidates: Vec<PathBuf>,
    ) -> Result<Vec<PathBuf>, FilesystemInvalidationError> {
        let ignored = git_ignored(self.git.repository_root(), &candidates)
            .await
            .map_err(|source| {
                FilesystemInvalidationError::with_source(
                    FilesystemInvalidationErrorType::Git,
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
            .filter(|candidate| !ignored.contains(candidate) && self.roots.is_input(candidate))
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
                FilesystemInvalidationErrorType::Watch,
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
                    FilesystemInvalidationErrorType::Read,
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
                            FilesystemInvalidationErrorType::Read,
                            self.git.repository_root(),
                            "cannot read filesystem notification",
                            source,
                        )
                    })?;
                }
                Err(_would_block) => {}
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
                if self.roots.owns_recursive_path(path) {
                    self.add_tree(path).await?;
                }
                if self.roots.exact_file_or_parent(path) {
                    self.add_exact_file_parents()?;
                }
            }
        }

        for (index, change) in changes.into_iter().enumerate() {
            let invalidation = if change.events.contains(ReadFlags::QUEUE_OVERFLOW) {
                tracing::debug!(
                    event_mask = change.events.bits(),
                    "development filesystem queue overflow requires rerun"
                );
                DevInvalidation::Rerun
            } else if vanished_directories.contains(&index) {
                DevInvalidation::Ignore
            } else if let Some(path) = change.path {
                // Path classification decides only relevance. Every relevant
                // change rebuilds and publishes through the product deployment path.
                if self.roots.is_input(&path) {
                    tracing::debug!(
                        path = %path.display(),
                        event_mask = change.events.bits(),
                        "development filesystem input requires rerun"
                    );
                    DevInvalidation::Rerun
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

    pub(crate) async fn replace_inputs(
        &mut self,
        inputs: WatchInputs,
    ) -> Result<(), FilesystemInvalidationError> {
        self.replace_roots(inputs.roots, inputs.shared).await?;
        self.replace_configuration_files(inputs.files)
    }

    pub(crate) async fn next(
        &mut self,
    ) -> Result<Option<DevInvalidation>, FilesystemInvalidationError> {
        if let Some(invalidation) = self.pending.pop_front() {
            return Ok(Some(invalidation));
        }
        self.read_batch().await?;
        Ok(self.pending.pop_front())
    }

    pub(crate) fn try_next(&mut self) -> Option<DevInvalidation> {
        self.pending.pop_front()
    }
}

fn read_subdirectories(directory: &Path) -> Result<Vec<PathBuf>, FilesystemInvalidationError> {
    let entries = fs::read_dir(directory).map_err(|source| {
        FilesystemInvalidationError::with_source(
            FilesystemInvalidationErrorType::Watch,
            directory,
            "cannot enumerate watched directory",
            source,
        )
    })?;
    let mut children = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|source| {
            FilesystemInvalidationError::with_source(
                FilesystemInvalidationErrorType::Watch,
                directory,
                "cannot read watched directory entry",
                source,
            )
        })?;
        let file_type = entry.file_type().map_err(|source| {
            FilesystemInvalidationError::with_source(
                FilesystemInvalidationErrorType::Watch,
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

fn read_changes(
    descriptor: &OwnedFd,
    watched_directories: &HashMap<i32, PathBuf>,
) -> io::Result<Vec<RawChange>> {
    let mut buffer = vec![MaybeUninit::uninit(); INOTIFY_BUFFER_BYTES].into_boxed_slice();
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

        /// The package sits inside a component root, so a package file the
        /// loop writes itself is also under a watched build root.
        fn package(&self) -> PathBuf {
            self.component().join("package")
        }

        fn component(&self) -> PathBuf {
            self.root.join("component")
        }

        fn write_fixture(&self) {
            fs::create_dir_all(self.package().join("query")).expect("create package query root");
            fs::create_dir_all(self.package().join("generated")).expect("create generated root");
            fs::create_dir_all(self.package().join("migrations")).expect("create migrations root");
            fs::create_dir_all(self.component().join("src")).expect("create component source root");
            fs::write(self.package().join("generated/wamn.json"), "{}")
                .expect("write package manifest");
            fs::write(self.package().join("query/widget_list.sql"), "SELECT 1")
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

    #[tokio::test]
    async fn a_watch_failure_retries_the_next_edit_and_shutdown_waits_for_the_active_pass()
    -> anyhow::Result<()> {
        let repository = TempRepository::new();
        repository.write_fixture();
        let root = repository.package();
        let git = GitSource::discover(&repository.root).await?;
        let source = FilesystemInvalidationSource::new([root.clone()], [], git).await?;
        let (shutdown, receiver) = tokio::sync::watch::channel(false);
        let mut passes = 0;
        tokio::time::timeout(
            Duration::from_secs(5),
            crate::dev::run(Some(source), receiver, async |_| {
                passes += 1;
                let pass = passes;
                let root = &root;
                let shutdown = &shutdown;
                {
                    if pass == 1 {
                        fs::write(root.join("query/widget_list.sql"), "SELECT 2")?;
                        anyhow::bail!("publication refused");
                    }
                    shutdown.send_replace(true);
                    tokio::task::yield_now().await;
                    Ok(())
                }
            }),
        )
        .await??;
        assert_eq!(
            passes, 2,
            "a queued edit retries the failed pass before shutdown"
        );
        Ok(())
    }

    #[tokio::test]
    async fn filesystem_events_rerun_for_source_and_ignore_output() {
        for explicit_component in [false, true] {
            let repository = TempRepository::new();
            repository.write_fixture();
            let root = repository.package();
            let mut inputs = vec![
                root.join("component/src/lib.rs"),
                root.join("data/src/lib.rs"),
                root.join("web/src/main.ts"),
                root.join("query/widget_list.sql"),
            ];
            if explicit_component {
                inputs.push(repository.component().join("src/lib.rs"));
            }
            for input in &inputs {
                fs::create_dir_all(input.parent().unwrap()).expect("create source directory");
                fs::write(input, "source").expect("write source");
            }
            let outputs = [
                "generated/components/app.wasm",
                "web/dist/index.html",
                "web/node_modules/module/index.js",
            ];
            for output in outputs {
                let path = root.join(output);
                fs::create_dir_all(path.parent().unwrap()).expect("create output directory");
                fs::write(path, "output").expect("write output");
            }
            let git = GitSource::discover(&repository.root)
                .await
                .expect("source repository");
            let components = explicit_component.then(|| repository.component());
            let mut source = FilesystemInvalidationSource::new([root.clone()], components, git)
                .await
                .expect("watch source inputs");
            for input in inputs {
                fs::write(&input, "edited").expect("edit source");
                assert!(
                    next_batch(&mut source)
                        .await
                        .contains(&DevInvalidation::Rerun),
                    "{input:?} must start a rebuild"
                );
            }
            for output in outputs {
                fs::write(root.join(output), "rebuilt").expect("rewrite output");
            }
            assert_eq!(
                next_batch_after_source_edit(&mut source, &root).await,
                [DevInvalidation::Rerun],
                "generated output must remain unwatched"
            );
        }
    }

    #[tokio::test]
    async fn configuration_files_are_observed_with_existing_or_absent_parents() {
        for parent_exists in [true, false] {
            let repository = TempRepository::new();
            repository.write_fixture();
            let git = GitSource::discover(&repository.root)
                .await
                .expect("source repository");
            let mut source = FilesystemInvalidationSource::new([repository.package()], [], git)
                .await
                .expect("watch package inputs");
            let configuration = TempRepository::new();
            let parent = configuration.root.join("local");
            let file = parent.join("privileges.sql");
            if parent_exists {
                fs::create_dir(&parent).expect("create configuration parent");
                fs::write(&file, "SELECT 1").expect("write configuration");
            }
            source
                .replace_configuration_files([file.clone()])
                .expect("watch exact configuration file");
            if !parent_exists {
                fs::create_dir(&parent).expect("create configuration parent");
                fs::write(&file, "SELECT 1").expect("write configuration");
                assert!(
                    next_batch(&mut source)
                        .await
                        .contains(&DevInvalidation::Rerun)
                );
            }
            fs::write(&file, "SELECT 2").expect("edit configuration");
            assert!(
                next_batch(&mut source)
                    .await
                    .contains(&DevInvalidation::Rerun)
            );
            let replacement = parent.join("replacement.sql");
            fs::write(&replacement, "SELECT 3").expect("write replacement");
            fs::rename(replacement, &file).expect("replace configuration atomically");
            assert!(
                next_batch(&mut source)
                    .await
                    .contains(&DevInvalidation::Rerun)
            );
            fs::remove_file(file).expect("remove configuration");
            assert!(
                next_batch(&mut source)
                    .await
                    .contains(&DevInvalidation::Rerun)
            );
        }
    }

    /// Inotify keeps event order, so the edit's rerun is the whole batch only
    /// when the writes before it raised no event.
    async fn next_batch_after_source_edit(
        source: &mut FilesystemInvalidationSource,
        package: &Path,
    ) -> Vec<DevInvalidation> {
        fs::write(package.join("query/widget_list.sql"), "SELECT 3").expect("edit source");
        next_batch(source).await
    }

    async fn next_batch(source: &mut FilesystemInvalidationSource) -> Vec<DevInvalidation> {
        let first = tokio::time::timeout(Duration::from_secs(2), source.next())
            .await
            .expect("an event arrives")
            .expect("read the event")
            .expect("the source stays open");
        let mut events = vec![first];
        while let Some(event) = source.try_next() {
            events.push(event);
        }
        events
    }

    #[tokio::test]
    async fn a_refreshed_closure_observes_a_new_dependency() {
        let repository = TempRepository::new();
        repository.write_fixture();
        let dependency = repository.root.join("new-dependency");
        fs::create_dir_all(dependency.join("src")).expect("create dependency");
        let input = dependency.join("src/lib.rs");
        fs::write(&input, "pub fn value() {}\n").expect("write dependency");
        let git = GitSource::discover(&repository.root)
            .await
            .expect("source repository");
        let mut source = FilesystemInvalidationSource::new(
            [repository.package()],
            [repository.component()],
            git,
        )
        .await
        .expect("watch original closure");
        source
            .replace_roots(
                vec![repository.package()],
                vec![repository.component(), dependency],
            )
            .await
            .expect("refresh dependency closure");
        fs::write(input, "pub fn changed() {}\n").expect("edit new dependency");
        assert!(
            next_batch(&mut source)
                .await
                .contains(&DevInvalidation::Rerun)
        );
    }

    #[tokio::test]
    async fn git_snapshot_covers_the_whole_worktree_and_excludes_ignored_files() {
        let repository = TempRepository::new();
        let initial = GitSource::discover(&repository.root)
            .await
            .expect("initial repository");
        assert_eq!(
            initial.snapshot().await.unwrap_err().error_type(),
            GitSourceErrorType::Inspect
        );
        repository.write_fixture();
        let source = GitSource::discover(repository.package())
            .await
            .expect("discover source repository");

        let clean = source.snapshot().await.expect("read clean source state");
        assert_eq!(clean.repository_root(), repository.root);
        assert_eq!(clean.state(), GitSourceState::Clean);
        assert!(!clean.source_commit().is_empty());

        fs::write(repository.root.join("ignored"), "ignored bytes").expect("write ignored file");
        assert_eq!(
            source.snapshot().await.expect("read ignored state").state(),
            GitSourceState::Clean
        );

        let untracked = repository.root.join("outside-package");
        fs::write(&untracked, "untracked bytes").expect("write untracked file");
        assert_eq!(
            source
                .snapshot()
                .await
                .expect("read untracked state")
                .state(),
            GitSourceState::Dirty
        );
        fs::remove_file(untracked).expect("remove untracked file");
    }

    #[tokio::test]
    async fn ignored_build_outputs_never_rerun_overlapping_component_roots() {
        for preexisting in [false, true] {
            let repository = TempRepository::new();
            repository.write_fixture();
            let components = repository.root.join("components");
            let no_std = components.join("no-std");
            fs::create_dir_all(no_std.join("guest/src")).expect("create nested workspace");
            fs::write(
                repository.root.join(".gitignore"),
                "ignored\n/components/target\n/components/no-std/target\n",
            )
            .expect("ignore build output");
            let outputs = [components.join("target"), no_std.join("target")];
            if preexisting {
                for output in &outputs {
                    fs::create_dir(output).expect("create output directory");
                }
            }
            let git = GitSource::discover(&repository.root)
                .await
                .expect("source repository");
            let mut source = FilesystemInvalidationSource::new(
                [repository.package()],
                [components, no_std],
                git,
            )
            .await
            .expect("watch overlapping roots");
            if preexisting {
                for output in &outputs {
                    fs::write(output.join("guest.wasm"), "artifact").expect("write build output");
                }
                assert_eq!(
                    next_batch_after_source_edit(&mut source, &repository.package()).await,
                    [DevInvalidation::Rerun],
                    "existing build output must remain unwatched"
                );
                for output in &outputs {
                    fs::remove_dir_all(output).expect("remove build output");
                }
            }
            for output in &outputs {
                // Cargo can rename its staging tree before the watcher drains it.
                let staging = output.with_file_name("targetbpz54B");
                fs::create_dir_all(staging.join("wasm32-wasip2/debug"))
                    .expect("create build output");
                fs::write(staging.join("wasm32-wasip2/debug/guest.wasm"), "artifact")
                    .expect("write build artifact");
                fs::rename(staging, output).expect("publish ignored output");
            }
            assert!(
                next_batch(&mut source)
                    .await
                    .iter()
                    .all(|event| *event == DevInvalidation::Ignore)
            );
            for output in &outputs {
                fs::write(output.join("wasm32-wasip2/debug/guest.wasm"), "rebuilt")
                    .expect("rewrite build artifact");
            }
            assert_eq!(
                next_batch_after_source_edit(&mut source, &repository.package()).await,
                [DevInvalidation::Rerun],
                "recreated build output must remain unwatched"
            );
        }
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
            next_batch(&mut source)
                .await
                .contains(&DevInvalidation::Rerun),
            "a watched source moved into output still disappeared from the build"
        );

        fs::rename(
            &archived_source,
            repository.package().join("generated/archive"),
        )
        .expect("move watched source into watched but ignored generated output");
        assert!(
            next_batch(&mut source)
                .await
                .contains(&DevInvalidation::Rerun),
            "registering the moved inode must not erase its previous source ownership"
        );

        fs::remove_file(repository.component().join("src/lib.rs"))
            .expect("delete another tracked source");
        assert!(
            next_batch(&mut source)
                .await
                .contains(&DevInvalidation::Rerun),
            "ordinary source deletion must still invalidate"
        );

        let new_source = repository.component().join("target-new-source");
        fs::create_dir(&new_source).expect("create a real target-prefixed source directory");
        fs::write(new_source.join("lib.rs"), "pub fn new_source() {}")
            .expect("write new source with no move into ignored output");
        assert!(
            next_batch(&mut source)
                .await
                .contains(&DevInvalidation::Rerun),
            "a target prefix alone must never hide authored source"
        );
    }
}
