use super::{
    BTreeMap, BTreeSet, Component, Graph, Package, Path, PathBuf, Value, bytes_sha256,
    canonical_json_bytes, ensure, json,
};
use anyhow::Context as _;

/// The paths that a build script declares with `cargo:rerun-if-changed`, or
/// `None` when it declares none or builds one at run time. Cargo runs the
/// script again only when a declared path changes, so the script reads no
/// other file outside its package.
pub(super) fn declared_reads(script: &str) -> Option<Vec<String>> {
    let mut reads = Vec::new();
    for (start, directive) in script.match_indices("rerun-if-changed=") {
        let rest = &script[start + directive.len()..];
        let path = &rest[..rest.find('"')?];
        if path.is_empty() || path.contains(['{', '}', '\\']) {
            return None;
        }
        reads.push(path.to_owned());
    }
    (!reads.is_empty()).then_some(reads)
}

/// The packages that building `roots` compiles, with the dev dependencies of
/// `cases` because their test executables need them.
pub(super) fn closure<'a>(
    graph: &'a Graph,
    roots: &[&str],
    cases: &[&str],
) -> anyhow::Result<Vec<&'a Package>> {
    let mut stack = Vec::new();
    for (names, dev) in [(roots, false), (cases, true)] {
        for name in names {
            let id = graph
                .names
                .get(*name)
                .with_context(|| format!("cargo metadata has no package {name}"))?;
            stack.push((id, dev));
        }
    }
    let mut seen = BTreeSet::new();
    let mut packages = Vec::new();
    while let Some((id, dev)) = stack.pop() {
        if !seen.insert(id) {
            continue;
        }
        let package = &graph.packages[id];
        for (dependency, dev_only) in &package.dependencies {
            if dev || !dev_only {
                stack.push((dependency, false));
            }
        }
        packages.push(package);
    }
    Ok(packages)
}

/// `relative` joined to the repository-relative `base`, or `None` when it
/// leaves the repository.
pub(super) fn resolve(base: &Path, relative: &str) -> Option<PathBuf> {
    let joined = base.join(relative);
    let mut path = Vec::new();
    for component in joined.components() {
        match component {
            Component::Normal(part) => path.push(part),
            Component::ParentDir => {
                path.pop()?;
            }
            Component::CurDir => {}
            Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    Some(path.iter().collect())
}

/// The string literals in `text`, without unescaping.
pub(super) fn literals(text: &str) -> Vec<&str> {
    let mut found = Vec::new();
    let mut start = None;
    let mut escaped = false;
    for (index, character) in text.char_indices() {
        match (start, character) {
            (Some(_), _) if escaped => escaped = false,
            (Some(_), '\\') => escaped = true,
            (Some(open), '"') => {
                found.push(&text[open..index]);
                start = None;
            }
            (None, '"') => start = Some(index + 1),
            _ => {}
        }
    }
    found
}

/// The invocation that starts at `text`: up to the `]` of an attribute, or
/// to the delimiter that closes the macro's first delimiter.
pub(super) fn invocation(text: &str) -> &str {
    let mut depth = 0_usize;
    let mut quoted = false;
    let mut escaped = false;
    for (index, character) in text.char_indices() {
        if quoted {
            match character {
                _ if escaped => escaped = false,
                '\\' => escaped = true,
                '"' => quoted = false,
                _ => {}
            }
            continue;
        }
        match character {
            '"' => quoted = true,
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return &text[..=index];
                }
            }
            _ => {}
        }
    }
    text
}

/// The context paths reachable through the target's stage dependencies.
/// Refuse unsupported source syntax instead of omitting an input.
pub(super) fn copied_paths(
    dockerfile: &str,
    target: &str,
) -> anyhow::Result<BTreeMap<PathBuf, bool>> {
    #[derive(Default)]
    struct Sources {
        dependencies: Vec<String>,
        paths: BTreeSet<PathBuf>,
        narrow: bool,
    }
    let mut stages = Vec::<Sources>::new();
    let mut names = BTreeMap::new();
    let mut instruction = String::new();
    for line in dockerfile.lines().map(str::trim) {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        instruction.push_str(line.trim_end_matches('\\'));
        if line.ends_with('\\') {
            instruction.push(' ');
            continue;
        }
        let (verb, arguments) = instruction
            .split_once(char::is_whitespace)
            .unwrap_or((&instruction, ""));
        if verb.eq_ignore_ascii_case("FROM") {
            let mut words = arguments
                .split_whitespace()
                .filter(|word| !word.starts_with("--"));
            let base = words.next().context("FROM has no image")?;
            let mut sources = Sources::default();
            sources.dependencies.push(base.to_owned());
            let index = stages.len();
            names.insert(index.to_string(), index);
            if let Some(as_word) = words.next() {
                ensure!(
                    as_word.eq_ignore_ascii_case("AS"),
                    "unsupported FROM: {instruction}"
                );
                let name = words.next().context("FROM AS has no name")?;
                sources.narrow = ["root-source", "component-toolchain"].contains(&name);
                names.insert(name.to_owned(), index);
            }
            stages.push(sources);
        } else if verb.eq_ignore_ascii_case("COPY") || verb.eq_ignore_ascii_case("ADD") {
            let sources = stages.last_mut().context("COPY or ADD before FROM")?;
            let mut rest = arguments.trim();
            let mut from = None;
            while rest.starts_with("--") {
                let (flag, remaining) = rest
                    .split_once(char::is_whitespace)
                    .context("COPY or ADD has no source")?;
                if let Some(stage) = flag.strip_prefix("--from=") {
                    from = Some(stage.to_owned());
                } else {
                    ensure!(flag.contains('='), "unsupported COPY or ADD flag: {flag}");
                }
                rest = remaining.trim_start();
            }
            if let Some(from) = from {
                sources.dependencies.push(from);
            } else {
                let paths: Vec<String> = if rest.starts_with('[') {
                    serde_json::from_str(rest).context("read COPY or ADD sources")?
                } else {
                    ensure!(
                        !rest.contains(['\'', '"', '\\']),
                        "use JSON for quoted COPY or ADD paths: {rest}"
                    );
                    rest.split_whitespace().map(str::to_owned).collect()
                };
                ensure!(
                    paths.len() >= 2,
                    "COPY or ADD needs sources and a destination"
                );
                for source in &paths[..paths.len() - 1] {
                    if verb.eq_ignore_ascii_case("ADD")
                        && (source.contains("://") || source.starts_with("git@"))
                    {
                        continue;
                    }
                    ensure!(
                        !source.contains(['*', '?', '[', '$']) && !source.starts_with("<<"),
                        "unsupported COPY or ADD source: {source}"
                    );
                    let path = resolve(Path::new(""), source.trim_start_matches('/'))
                        .with_context(|| {
                            format!("COPY or ADD source leaves the context: {source}")
                        })?;
                    sources.paths.insert(path);
                }
            }
        }
        instruction.clear();
    }
    ensure!(instruction.is_empty(), "unfinished Dockerfile instruction");
    let mut pending = vec![
        *names
            .get(target)
            .with_context(|| format!("no Dockerfile target {target}"))?,
    ];
    let mut seen = BTreeSet::new();
    let mut paths = BTreeMap::new();
    while let Some(index) = pending.pop() {
        if !seen.insert(index) {
            continue;
        }
        for path in &stages[index].paths {
            // Only these source stages use the Cargo graph to narrow roots.
            // A direct image copy reads the complete source path.
            let whole = !stages[index].narrow
                || ![
                    "crates",
                    "apps",
                    "services",
                    "test-support",
                    "tests",
                    "deploy",
                ]
                .iter()
                .any(|root| path == Path::new(root));
            *paths.entry(path.clone()).or_insert(false) |= whole;
        }
        for dependency in &stages[index].dependencies {
            if let Some(index) = names.get(dependency) {
                pending.push(*index);
            }
        }
    }
    Ok(paths)
}

/// The hex SHA-256 of `target`'s build arguments and input files.
pub(in super::super) fn input_hash(target: &str, files: &BTreeMap<String, String>) -> String {
    let document = json!({
        "format": "wamn-input-hash/1",
        "target": target,
        // The checkout key names cache mounts only, so it is left out.
        "arguments": super::super::docker_arguments("", "Dockerfile", (target == "runner").then_some(Path::new("apps-target"))),
        "labels": ["wamn.dev/build-profile=release"],
        "files": files,
    });
    let digest = bytes_sha256(&canonical_json_bytes(&document));
    digest.trim_start_matches("sha256:").to_owned()
}

pub(super) fn metadata_graph(
    metadata: &Value,
    mut packages: BTreeMap<String, Package>,
    names: BTreeMap<String, String>,
) -> anyhow::Result<Graph> {
    let text = |value: &Value| value.as_str().map(str::to_owned);
    for node in metadata["resolve"]["nodes"]
        .as_array()
        .context("metadata resolve nodes")?
    {
        let Some(package) = node["id"]
            .as_str()
            .and_then(|id| packages.contains_key(id).then(|| id.to_owned()))
        else {
            continue;
        };
        let mut dependencies = Vec::new();
        for dependency in node["deps"].as_array().context("node dependencies")? {
            let id = text(&dependency["pkg"]).context("a dependency id")?;
            if !packages.contains_key(&id) {
                continue;
            }
            let dev = dependency["dep_kinds"]
                .as_array()
                .context("dependency kinds")?
                .iter()
                .all(|kind| kind["kind"] == "dev");
            dependencies.push((id, dev));
        }
        if let Some(package) = packages.get_mut(&package) {
            package.dependencies = dependencies;
        }
    }
    Ok(Graph { packages, names })
}
