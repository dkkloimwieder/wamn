//! Guards the shared bench linker helper against `flowrunner` world drift.
//!
//! `instantiate_pre` fails outright when a world import is unlinked, so every
//! addition to `components/execution/flowrunner/wit/world.wit` must reach
//! `tests/integration/src/flowrunner_linker.rs`. Twice it did not: the
//! `wamn:runner/http-effect` addition broke three harnesses for 17 days because
//! nothing asserted registration completeness. This diffs the two files.

/// The `flowrunner` world's import set, source of truth.
const WORLD_WIT: &str = include_str!("../../../components/execution/flowrunner/wit/world.wit");

/// The single registration point for every hand-rolled `flowrunner` linker.
const LINKER_SOURCE: &str = include_str!("../../../tests/integration/src/flowrunner_linker.rs");

/// Each world import mapped to the helper call that links it. This table is the
/// second half of the guard: a NEW world import has no entry, so the guard names
/// it instead of silently passing. Several imports share one registration call
/// (a host plugin links a whole interface group), and one call may back imports
/// the world does not declare (`wasmtime_wasi` links all of WASI p2) — the
/// mapping is import -> the call that must be present, not a bijection.
const REGISTRATIONS: &[(&str, &str)] = &[
    ("wamn:postgres/types@0.1.0", "wamn_postgres::add_to_linker"),
    ("wamn:postgres/client@0.1.0", "wamn_postgres::add_to_linker"),
    (
        "wamn:runner/plan-supply@0.1.0",
        "runner_plan_supply::add_to_linker",
    ),
    (
        "wamn:runner/causation@0.1.0",
        "wamn_postgres::add_runner_causation_to_linker",
    ),
    (
        "wamn:runner/http-effect@0.1.0",
        "connection_http::add_to_linker",
    ),
    (
        "wasi:logging/logging@0.1.0-draft",
        "wamn_logging::add_to_linker",
    ),
];

/// Drop `//` line comments so a commented-out import or registration counts as
/// absent and prose about an interface never counts as present. Neither file
/// carries a string literal containing `//`.
fn uncommented(source: &str) -> String {
    source
        .lines()
        .map(|line| line.split_once("//").map_or(line, |(code, _)| code))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The interfaces `world flowrunner` imports, in declaration order.
fn declared_imports(wit: &str) -> Vec<String> {
    let uncommented = uncommented(wit);
    let (_, body) = uncommented
        .split_once("world flowrunner {")
        .expect("flowrunner world.wit must declare `world flowrunner {`");
    body.lines()
        .filter_map(|line| Some(line.trim().strip_prefix("import ")?.trim()))
        .map(|import| import.trim_end_matches(';').trim().to_string())
        .collect()
}

/// The `path::add_*_to_linker*(` calls the helper makes. A call is a `::`-scoped
/// path (the helper's own `add_flowrunner_imports_to_linker` definition is not),
/// so the registrations are exactly the plugin entry points it invokes.
fn registration_calls(source: &str) -> Vec<String> {
    let uncommented = uncommented(source);
    let mut calls = Vec::new();
    for (open, _) in uncommented.match_indices('(') {
        let mut path = uncommented[..open]
            .chars()
            .rev()
            .take_while(|character| {
                character.is_ascii_alphanumeric() || *character == '_' || *character == ':'
            })
            .collect::<Vec<_>>();
        path.reverse();
        let path = path.into_iter().collect::<String>();
        let path = path.trim_start_matches(':').to_string();
        if path.contains("to_linker") && path.contains("::") && !calls.contains(&path) {
            calls.push(path);
        }
    }
    calls
}

/// Every way the helper can disagree with the world, one actionable line each.
fn complaints(wit: &str, linker: &str) -> Vec<String> {
    let imports = declared_imports(wit);
    let calls = registration_calls(linker);
    let mut complaints = Vec::new();
    let mut required = Vec::new();

    for import in &imports {
        let Some((_, call)) = REGISTRATIONS
            .iter()
            .find(|(mapped, _)| mapped == &import.as_str())
        else {
            complaints.push(format!(
                "world imports `{import}`, which this guard's REGISTRATIONS table does not map \
                 to a linker call — register it in tests/integration/src/flowrunner_linker.rs \
                 and add the mapping here"
            ));
            continue;
        };
        if !required.contains(call) {
            required.push(*call);
        }
        if !calls.iter().any(|made| made.as_str() == *call) {
            complaints.push(format!(
                "world imports `{import}`, but tests/integration/src/flowrunner_linker.rs never \
                 calls `{call}` — every bench linker built through that helper will fail \
                 instantiate_pre on the unlinked import"
            ));
        }
    }

    for call in &calls {
        if !required.contains(&call.as_str()) {
            complaints.push(format!(
                "tests/integration/src/flowrunner_linker.rs calls `{call}`, which no `world \
                 flowrunner` import requires — drop the stale registration or restore the import"
            ));
        }
    }

    complaints
}

#[test]
fn flowrunner_world_imports_match_the_shared_linker_registrations() {
    let imports = declared_imports(WORLD_WIT);
    assert!(
        imports.len() == 6 && imports.iter().all(|import| import.contains(':')),
        "world.wit import parse returned {imports:?} — the WIT layout changed under the guard"
    );
    assert!(
        registration_calls(LINKER_SOURCE).len() == 5,
        "flowrunner_linker.rs registration parse found too few calls — the helper layout \
         changed under the guard"
    );

    let complaints = complaints(WORLD_WIT, LINKER_SOURCE);
    assert!(
        complaints.is_empty(),
        "flowrunner world and the shared bench linker have drifted:\n- {}",
        complaints.join("\n- ")
    );
}

/// `wamn:runner/http-effect` is the runner's only egress, and its host plugin
/// builds and drops one HTTP client per invocation. The wash-runtime pooled
/// `wasi:http` path keys its connection pool by `workload_id` alone, so two
/// credential generations inside one run would silently share a connection if
/// the world ever imported it. Keeping `wasi:http` out of the world is what
/// makes that pool unreachable, so pin it here rather than trusting the
/// registration table above (which maps only imports the world already has).
#[test]
fn flowrunner_world_imports_no_wasi_http_interface() {
    let imports = declared_imports(WORLD_WIT);
    assert!(
        !imports.iter().any(|import| import.starts_with("wasi:http")),
        "flowrunner world imports {imports:?} — a `wasi:http` import reaches the wash-runtime \
         pooled outgoing-handler, whose pool is keyed by workload_id alone, so one run's \
         credential generations would share a connection"
    );

    let mutant = WORLD_WIT.replace(
        "world flowrunner {",
        "world flowrunner {\n  import wasi:http/outgoing-handler@0.2.3;",
    );
    assert!(
        declared_imports(&mutant)
            .iter()
            .any(|import| import.starts_with("wasi:http")),
        "the guard must see a `wasi:http` import added to the flowrunner world"
    );
}

#[test]
fn guard_names_a_new_world_import_the_helper_does_not_register() {
    let mutant = WORLD_WIT.replace(
        "world flowrunner {",
        "world flowrunner {\n  import wamn:runner/new-effect@0.1.0;",
    );

    let complaints = complaints(&mutant, LINKER_SOURCE);
    assert_eq!(complaints.len(), 1, "{complaints:?}");
    assert!(
        complaints[0].contains("wamn:runner/new-effect@0.1.0")
            && complaints[0].contains("flowrunner_linker.rs"),
        "{complaints:?}"
    );
}

#[test]
fn guard_names_a_mapped_import_whose_registration_was_removed() {
    let call = "connection_http::add_to_linker";
    assert!(LINKER_SOURCE.contains(call));
    let mutant = LINKER_SOURCE.replace(&format!("{call}(linker)?;"), "");

    let complaints = complaints(WORLD_WIT, &mutant);
    assert_eq!(complaints.len(), 1, "{complaints:?}");
    assert!(
        complaints[0].contains("wamn:runner/http-effect@0.1.0") && complaints[0].contains(call),
        "{complaints:?}"
    );
}

#[test]
fn guard_names_a_registration_no_world_import_requires() {
    let mutant = LINKER_SOURCE.replace(
        "    Ok(())",
        "    stale_plugin::add_to_linker(linker)?;\n    Ok(())",
    );

    let complaints = complaints(WORLD_WIT, &mutant);
    assert_eq!(complaints.len(), 1, "{complaints:?}");
    assert!(
        complaints[0].contains("stale_plugin::add_to_linker"),
        "{complaints:?}"
    );
}

#[test]
fn guard_reads_code_not_comments_or_formatting() {
    let commented_world = WORLD_WIT.replace(
        "world flowrunner {",
        "world flowrunner {\n  // import wamn:runner/new-effect@0.1.0;",
    );
    assert!(complaints(&commented_world, LINKER_SOURCE).is_empty());

    let commented_registration = LINKER_SOURCE.replace(
        "    connection_http::add_to_linker(linker)?;",
        "    // connection_http::add_to_linker(linker)?;",
    );
    let commented = complaints(WORLD_WIT, &commented_registration);
    assert_eq!(commented.len(), 1, "{commented:?}");
    assert!(
        commented[0].contains("wamn:runner/http-effect@0.1.0"),
        "{commented:?}"
    );

    let respaced = LINKER_SOURCE.replace(
        "    connection_http::add_to_linker(linker)?;",
        "    connection_http::add_to_linker(\n        linker,\n    )?;",
    );
    assert!(complaints(WORLD_WIT, &respaced).is_empty());
}
