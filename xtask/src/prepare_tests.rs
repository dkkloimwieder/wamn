//! Build application fixtures and the pinned ingress guest.

use anyhow::{Context as _, Result, ensure};
use std::collections::{BTreeMap, VecDeque};
use std::path::{Path, PathBuf};
use wamn_build::{BuildMode, BuildRequest, DescribeMode, default_output_root, package_output};

/// The application roots: each directory under `apps/` with a `wamn.k`.
pub(super) fn applications(root: &Path) -> Result<Vec<PathBuf>> {
    let mut applications = std::fs::read_dir(root.join("apps"))?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<std::io::Result<Vec<_>>>()?;
    applications.retain(|path| path.join("wamn.k").is_file());
    applications.sort_unstable();
    Ok(applications)
}

pub(super) async fn run(root: &Path) -> Result<()> {
    let mut pending = VecDeque::new();
    for path in applications(root)? {
        let source = wamn_build::prepare_source(&path)
            .await
            .with_context(|| format!("read application {}", path.display()))?;
        pending.push_back(source);
    }
    pending
        .make_contiguous()
        .sort_by_key(|source| !source.bases().is_empty());

    let mut built = BTreeMap::new();
    while !pending.is_empty() {
        let ready = pending.iter().position(|source| {
            source
                .bases()
                .values()
                .all(|base| built.contains_key(&base.package))
        });
        let Some(source) = ready.and_then(|index| pending.remove(index)) else {
            let waiting = pending
                .iter()
                .map(|source| source.package().id.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            anyhow::bail!("missing or cyclic application base dependencies for {waiting}");
        };
        let output_root = default_output_root(source.root());
        let request = BuildRequest::new(source.root(), &output_root, DescribeMode::Cached);
        let result = wamn_build::build_prepared(&request, &source, BuildMode::Published)
            .await
            .with_context(|| format!("build application {}", source.package().id))?;
        let id = result.package.id;
        let output = package_output(&output_root, &id);
        eprintln!("Prepared {} at {}", id, output.display());
        built.insert(id, output);
    }

    let ingress = wamn_build::build_component(root, &root.join("guests/ingress/Cargo.toml"))
        .await
        .context("build pinned ingress component")?;
    // The component consumers name these variables and components.
    let mut exports = Vec::new();
    for (name, id, component) in [
        (
            "WAMN_EFFECTIVE_RELEASE_BASE_COMPONENT_WASM",
            "platform_fixture",
            "fixture.wasm",
        ),
        (
            "WAMN_EFFECTIVE_RELEASE_OVERLAY_COMPONENT_WASM",
            "platform_fixture_overlay",
            "fixture_overlay.wasm",
        ),
        ("WAMN_EDGE_DEVICE_COMPONENT", "edge_device", "device.wasm"),
    ] {
        let output = built
            .get(id)
            .with_context(|| format!("missing application {id}"))?;
        exports.push((name, output.join("components").join(component)));
    }
    exports.push(("WAMN_INGRESS_COMPONENT", ingress));
    let mut lines = Vec::new();
    for (name, path) in exports {
        ensure!(path.is_file(), "missing {name} at {}", path.display());
        let value = path.to_str().context("component path is not valid UTF-8")?;
        lines.push(format!("export {name}='{}'", value.replace('\'', "'\\''")));
    }
    for line in lines {
        println!("{line}");
    }
    Ok(())
}
