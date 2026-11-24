use anyhow::Context as _;
use cargo_metadata::{CargoOpt, DependencyKind, MetadataCommand};
use std::collections::{HashMap, HashSet, VecDeque, hash_map::Entry};
use std::path::Path;

pub(super) fn report(
    manifest: &Path,
    refused: impl Fn(&str, &str, bool) -> bool,
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
        let first = (start, false);
        let mut parents = HashMap::from([(first, None)]);
        let mut pending = VecDeque::from([first]);
        let mut reported = HashSet::new();
        while let Some(step @ (id, runtime_exempt)) = pending.pop_front() {
            let name = names[id];
            if id != start && refused(source, name, runtime_exempt) && reported.insert(name) {
                let mut path: Vec<_> = std::iter::successors(Some(step), |step| parents[step])
                    .map(|(id, _)| names[id])
                    .collect();
                path.reverse();
                failures.push(format!(
                    "{source} links forbidden dependency {name}: {}",
                    path.join(" -> ")
                ));
            }
            for dep in &nodes[id].deps {
                // permanent: stock address parsing and egress policy (ruling Q-A). See Open items.
                let next = (
                    &dep.pkg,
                    runtime_exempt || (source == "wamn-control" && names[&dep.pkg] == "wamn-http"),
                );
                if dep
                    .dep_kinds
                    .iter()
                    .any(|edge| edge.kind == DependencyKind::Normal)
                    && let Entry::Vacant(entry) = parents.entry(next)
                {
                    entry.insert(Some(step));
                    pending.push_back(next);
                }
            }
        }
    }
    Ok(())
}
