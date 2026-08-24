//! Guards the dispatcher, executor, and waker privilege split.

use std::fs;
use std::path::{Path, PathBuf};

use wamn_run_state::queue::parked_due_sql;

const DISPATCHER_SOURCE: &str = "services/dispatcher/src/lib.rs";
const DISPATCHER_MAIN: &str = "services/dispatcher/src/main.rs";
const DISPATCHER_MANIFEST: &str = "services/dispatcher/Cargo.toml";
const DISPATCHER_DEPLOYMENT: &str = "deploy/platform/dispatcher.yaml";
const EXECUTION_HOST_SOURCE: &str = "crates/execution/host/src/lib.rs";
const RUN_STATE_SQL_SOURCE: &str = "crates/execution/run-state/src/sql.rs";
const RUN_STATE_QUEUE_SQL_SOURCE: &str = "crates/execution/run-state/src/queue/sql.rs";
const RUN_STATE_TRANSITIONS_SOURCE: &str = "crates/execution/run-state/src/transitions.rs";
const FRESH_RUN_STATE_CARRIERS: &[&str] = &[
    "deploy/sql/run-state.sql",
    "deploy/sql/postgres-init.sql",
    "tests/conformance/src/schema_drift.rs",
    "crates/platform/runtime/tests/common/mod.rs",
];
const EXECUTOR_SOURCE: &str = "services/executor/src/lib.rs";
const EXECUTOR_MANIFEST: &str = "services/executor/Cargo.toml";
const WAKER_SOURCE: &str = "services/waker/src/lib.rs";
const WAKER_MANIFEST: &str = "services/waker/Cargo.toml";
const WAKER_DEPLOYMENT: &str = "deploy/platform/waker.yaml";

fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("conformance package must live at tests/conformance")
        .to_path_buf()
}

fn read(relative_path: &str) -> String {
    let path = repository_root().join(relative_path);
    fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()))
}

fn normalize(source: &str) -> String {
    source.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn assert_read_only_select(sql: &str, label: &str) {
    let uppercase = sql.to_ascii_uppercase();
    assert!(
        uppercase.trim_start().starts_with("SELECT "),
        "{label} must remain a SELECT"
    );
    for forbidden in [
        "INSERT ",
        "UPDATE ",
        "DELETE ",
        "MERGE ",
        "CALL ",
        "COPY ",
        "LOCK ",
        "TRUNCATE ",
        "ALTER ",
        "DROP ",
        "CREATE ",
    ] {
        assert!(
            !uppercase.contains(forbidden),
            "{label} gained mutating SQL {forbidden:?}"
        );
    }
}

fn rust_string_constant(source: &str, start: &str, end: &str) -> String {
    let declaration = section(source, start, end);
    let (_, value) = declaration
        .split_once('=')
        .unwrap_or_else(|| panic!("missing value for {start:?}"));
    value
        .trim()
        .trim_end_matches(';')
        .replace(['"', '\\'], "")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn without_yaml_comments(source: &str) -> String {
    source
        .lines()
        .map(|line| line.split_once('#').map_or(line, |(contents, _)| contents))
        .collect::<Vec<_>>()
        .join("\n")
}

fn grants_scale_authority(document: &str) -> bool {
    let document = without_yaml_comments(document);
    let is_rbac_role = document
        .lines()
        .any(|line| matches!(line.trim(), "kind: Role" | "kind: ClusterRole"));
    let lower = document.to_ascii_lowercase();
    let deployment_resource = lower.contains("deployments") || document.contains('*');
    let mutating_verb =
        lower.contains("patch") || lower.contains("update") || document.contains('*');
    is_rbac_role && deployment_resource && mutating_verb
}

fn section<'a>(source: &'a str, start: &str, end: &str) -> &'a str {
    let (_, remainder) = source
        .split_once(start)
        .unwrap_or_else(|| panic!("missing section start {start:?}"));
    remainder
        .split_once(end)
        .map_or(remainder, |(contents, _)| contents)
}

#[test]
fn dispatcher_reconciliation_is_tenant_scoped_and_read_only() {
    let reconciliation_sql = parked_due_sql(64);
    assert_read_only_select(&reconciliation_sql, "dispatcher reconciliation");
    assert!(reconciliation_sql.starts_with("SELECT q.run_id FROM run_queue AS q"));
    for required in [
        "q.tenant_id = current_setting('app.tenant', true)",
        "q.available_at <= now() + interval '250 milliseconds'",
        "q.lease_expires_at IS NULL OR q.lease_expires_at <= now()",
        "ORDER BY q.available_at, q.stream_seq, q.run_id",
        "LIMIT 64",
    ] {
        assert!(
            reconciliation_sql.contains(required),
            "dispatcher reconciliation lost required SQL boundary {required:?}"
        );
    }
    assert!(
        !reconciliation_sql.contains("FOR UPDATE"),
        "dispatcher reconciliation must not lock queue rows"
    );

    let dispatcher = read(DISPATCHER_SOURCE);
    let queue_depth_sql = rust_string_constant(
        &dispatcher,
        "pub const RUN_QUEUE_DEPTH_SQL",
        "/// [9.8] One project's last-sampled claimable queue depth",
    );
    assert_read_only_select(&queue_depth_sql, "queue-depth sample");
    for required in [
        "q.tenant_id = current_setting('app.tenant', true)",
        "q.available_at <= now()",
        "q.lease_expires_at IS NULL OR q.lease_expires_at <= now()",
        "q.attempts < q.max_attempts",
    ] {
        assert!(
            queue_depth_sql.contains(required),
            "queue-depth sample lost claim predicate {required:?}"
        );
    }

    assert_eq!(
        dispatcher.matches(".batch_execute(").count(),
        1,
        "only the tenant/search-path session setup may use batch_execute"
    );
    assert_eq!(
        dispatcher.matches(".query(").count(),
        1,
        "dispatcher may execute only the parked-due SELECT query"
    );
    assert_eq!(
        dispatcher.matches(".query_one(").count(),
        1,
        "dispatcher may execute only the queue-depth SELECT query_one"
    );
    assert!(
        dispatcher.contains("use wamn_run_state::queue::parked_due_sql;"),
        "dispatcher must import only the read-only run-state reconciliation builder"
    );
    for forbidden in [
        ".execute(",
        ".transaction(",
        ".query_raw(",
        ".simple_query(",
        ".copy_in(",
        ".copy_out(",
        "claim_next_production",
        "grant_production_claim_sql",
        "reset_pre_effect_claim_sql",
        "terminalize_effect_uncertain_claim_sql",
        "terminalize_resolution_refusal_claim_sql",
        "terminalize_exhausted_production_sql",
        "mark_running_sql",
        "dequeue_sql",
        "enqueue_sql",
        "ScaleClient",
    ] {
        assert!(
            !dispatcher.contains(forbidden),
            "dispatcher gained forbidden transition or scaling surface {forbidden:?}"
        );
    }

    let dispatcher_main = read(DISPATCHER_MAIN);
    let dispatcher_manifest = read(DISPATCHER_MANIFEST);
    let dispatcher_deployment = read(DISPATCHER_DEPLOYMENT);
    let dispatcher_boundary = [
        dispatcher.as_str(),
        dispatcher_main.as_str(),
        dispatcher_manifest.as_str(),
        dispatcher_deployment.as_str(),
    ]
    .join("\n")
    .to_ascii_lowercase();
    for retired in [
        "cancel",
        "partition_key",
        "partition_policy",
        "partition_owner",
        "run_dead_letters",
    ] {
        assert!(
            !dispatcher_boundary.contains(retired),
            "production dispatcher boundary retains retired vocabulary {retired:?}"
        );
    }

    let tick = normalize(section(
        &dispatcher,
        "pub async fn tick_project",
        "/// Tick each project",
    ));
    for required in [
        "p.client.query(&parked_due_sql(batch), &[]).await?",
        "p.client.query_one(RUN_QUEUE_DEPTH_SQL, &[]).await?.get(0)",
        "if let Some(nats) = nats && !doorbells.is_empty()",
        "nats.publish(subject.clone(), run_id.into_bytes().into()) .await?",
        "nats.flush().await?",
        "cadence.next_interval(p.interval_ms, report.found_work())",
    ] {
        assert!(
            tick.contains(required),
            "dispatcher tick lost reconciliation/doorbell boundary {required:?}"
        );
    }

    let run = normalize(section(
        &dispatcher,
        "pub async fn run(args: DispatchArgs)",
        "#[derive(Debug, serde::Deserialize)]",
    ));
    for required in [
        "let nats = match connect_nats",
        "Ok(c) => Some(c)",
        "Err(e) =>",
        "None",
        "doorbell hints disabled, reconciliation sweeps still guarantee pickup",
    ] {
        assert!(
            run.contains(required),
            "NATS must remain a best-effort hint path: missing {required:?}"
        );
    }
}

#[test]
fn guest_safe_run_state_surface_has_no_raw_projection_mutation() {
    for path in [
        RUN_STATE_SQL_SOURCE,
        RUN_STATE_QUEUE_SQL_SOURCE,
        RUN_STATE_TRANSITIONS_SOURCE,
    ] {
        let source = read(path);
        for forbidden in [
            "INSERT INTO node_runs",
            "UPDATE node_runs",
            "insert_node_run_success",
            "insert_node_run_error",
            "reserved_checkpoint_sql",
            "complete_attempt_success_sql",
            "complete_attempt_error_sql",
        ] {
            assert!(
                !source.contains(forbidden),
                "guest-safe source {path} retained projection mutation {forbidden:?}"
            );
        }
    }
}

#[test]
fn retired_uncalled_run_builders_stay_deleted_while_park_remains() {
    let run_state_sql = read(RUN_STATE_SQL_SOURCE);
    for retired in [
        "pub fn insert_run_sql(",
        "pub fn insert_run_returning_id_sql(",
        "pub fn update_run_running_sql(",
        "pub fn update_run_failed_sql(",
        "pub fn update_run_completed(",
        "pub fn update_run_completed_sql(",
        "pub fn select_run_dispatch_sql(",
    ] {
        assert!(
            !run_state_sql.contains(retired),
            "run-state SQL restored retired builder {retired:?}"
        );
    }
    for retired_column in ["fail_node", "fail_reason"] {
        assert!(
            !run_state_sql.contains(retired_column),
            "run-state SQL restored retired failure surface {retired_column:?}"
        );
    }
}

#[test]
fn fresh_run_state_carriers_omit_retired_failure_detail_columns() {
    for path in FRESH_RUN_STATE_CARRIERS {
        let source = read(path);
        for retired_column in ["fail_node", "fail_reason"] {
            assert!(
                !source.contains(retired_column),
                "fresh run-state carrier {path} restored retired failure detail {retired_column:?}"
            );
        }
    }
}

#[test]
fn waker_alone_holds_scaling_authority_without_project_database_authority() {
    let dispatcher_deployment = read(DISPATCHER_DEPLOYMENT);
    let waker_deployment = read(WAKER_DEPLOYMENT);
    let waker_manifest = read(WAKER_MANIFEST);
    let waker_source = read(WAKER_SOURCE);

    assert!(dispatcher_deployment.contains("automountServiceAccountToken: false"));
    assert!(!dispatcher_deployment.contains("serviceAccountName:"));
    assert!(!dispatcher_deployment.contains("deployments/scale"));

    for required in [
        "serviceAccountName: waker",
        "automountServiceAccountToken: true",
        "resources: [\"deployments\", \"deployments/scale\"]",
        "verbs: [\"get\", \"patch\"]",
    ] {
        assert!(
            waker_deployment.contains(required),
            "waker deployment lost exact scaling authority {required:?}"
        );
    }

    for forbidden in [
        "tokio-postgres",
        "wamn-run-state",
        "wamn-pg-core",
        "WAMN_PG_URL",
        "DATABASE_URL",
        "wamn-dispatch-projects",
        "wamn-runner-db",
    ] {
        assert!(
            !waker_manifest.contains(forbidden)
                && !waker_source.contains(forbidden)
                && !waker_deployment.contains(forbidden),
            "waker gained project database authority marker {forbidden:?}"
        );
    }

    let role_document = waker_deployment
        .split("\n---\n")
        .find(|document| {
            without_yaml_comments(document)
                .lines()
                .any(|line| line.trim() == "kind: Role")
        })
        .expect("waker manifest must contain its namespace-scoped Role");
    let (_, rules) = role_document
        .split_once("rules:")
        .expect("waker Role must contain rules");
    assert_eq!(
        normalize(rules),
        "- apiGroups: [\"apps\"] resources: [\"deployments\", \"deployments/scale\"] verbs: [\"get\", \"patch\"]",
        "waker Role must contain exactly the scale get+patch grant"
    );

    let platform = repository_root().join("deploy/platform");
    let mut scale_holders = fs::read_dir(&platform)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", platform.display()))
        .map(|entry| entry.expect("platform directory entry"))
        .filter(|entry| {
            entry
                .path()
                .extension()
                .is_some_and(|extension| extension == "yaml")
        })
        .flat_map(|entry| {
            let contents = fs::read_to_string(entry.path()).expect("read platform manifest");
            let file_name = entry.file_name().to_string_lossy().into_owned();
            contents
                .split("\n---\n")
                .enumerate()
                .filter(|(_, document)| grants_scale_authority(document))
                .map(move |(index, _)| format!("{file_name}#{}", index + 1))
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    scale_holders.sort();
    assert_eq!(scale_holders, ["waker.yaml#2"]);
}
