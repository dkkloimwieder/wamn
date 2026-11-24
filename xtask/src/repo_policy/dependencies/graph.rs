use anyhow::Context as _;
use cargo_metadata::{CargoOpt, DependencyKind, MetadataCommand};
use std::collections::{HashMap, HashSet, VecDeque, hash_map::Entry};
use std::path::Path;

pub(super) fn report(
    manifest: &Path,
    refused: impl Fn(&str, &str) -> bool,
    failures: &mut Vec<String>,
) -> anyhow::Result<()> {
    let metadata = MetadataCommand::new()
        .manifest_path(manifest)
        .features(CargoOpt::AllFeatures)
        .other_options(vec!["--locked".into(), "--offline".into()])
        .exec()
        .with_context(|| format!("resolve dependencies from {}", manifest.display()))?;
    let resolve = metadata.resolve.as_ref().context("resolved dependencies")?;
    let nodes: HashMap<_, _> = resolve.nodes.iter().map(|node| (&node.id, node)).collect();
    let names: HashMap<_, _> = metadata
        .packages
        .iter()
        .map(|package| (&package.id, package.name.as_str()))
        .collect();
    for start in &metadata.workspace_members {
        let source = names[start];
        let mut parents = HashMap::from([(start, None)]);
        let mut pending = VecDeque::from([start]);
        let mut reported = HashSet::new();
        while let Some(id) = pending.pop_front() {
            let name = names[id];
            if id != start && refused(source, name) && reported.insert(name) {
                let mut path: Vec<_> = std::iter::successors(Some(id), |id| parents[id])
                    .map(|id| names[id])
                    .collect();
                path.reverse();
                failures.push(format!(
                    "{source} links forbidden dependency {name}: {}",
                    path.join(" -> ")
                ));
            }
            for dep in &nodes[id].deps {
                if dep
                    .dep_kinds
                    .iter()
                    .any(|edge| edge.kind == DependencyKind::Normal)
                    && let Entry::Vacant(entry) = parents.entry(&dep.pkg)
                {
                    entry.insert(Some(id));
                    pending.push_back(&dep.pkg);
                }
            }
        }
    }
    Ok(())
}
