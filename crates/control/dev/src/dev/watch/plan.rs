use super::{PackageRoot, Path, PathBuf, WatchRoots, is_input_path};

impl PackageRoot {
    pub(super) fn is_input(&self, path: &Path) -> bool {
        is_input_path(&self.root, path)
    }

    pub(super) fn owns_generated(&self, path: &Path) -> bool {
        path.starts_with(wamn_compiler::output_root(&self.root))
    }
}

impl WatchRoots {
    pub(super) fn exact_file_or_parent(&self, path: &Path) -> bool {
        self.configuration_files
            .iter()
            .any(|file| file == path || file.starts_with(path))
    }

    pub(super) fn owns_recursive_path(&self, path: &Path) -> bool {
        self.watched_roots().any(|root| path.starts_with(root))
    }

    pub(super) fn is_excluded(&self, path: &Path) -> bool {
        self.excluded
            .iter()
            .any(|directory| path == directory || path.starts_with(directory))
    }

    pub(super) fn is_input(&self, path: &Path) -> bool {
        // An excluded directory is unwatched, so nothing inside it can arrive
        // here; the directory ITSELF still can, through its watched parent,
        // when a build creates or replaces it.
        if self.is_excluded(path) {
            return false;
        }
        if self
            .packages
            .iter()
            .any(|package| package.owns_generated(path))
        {
            return false;
        }
        self.packages.iter().any(|package| package.is_input(path))
            || self
                .component_build_roots
                .iter()
                .any(|root| is_input_path(root, path))
            || self.exact_file_or_parent(path)
    }

    pub(super) fn watched_roots(&self) -> impl Iterator<Item = &Path> {
        self.packages
            .iter()
            .map(|package| package.root.as_path())
            .chain(self.component_build_roots.iter().map(PathBuf::as_path))
    }
}
