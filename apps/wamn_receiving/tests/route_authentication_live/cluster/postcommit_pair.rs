//! Compare the unchanged overlay across two independent Receiving installations.

use std::fs;
use std::path::Path;

use anyhow::{Context as _, ensure};
use serde_json::{Value, json};
use wamn_gate_harness::journey::BaseCandidate;

#[tokio::test]
#[ignore = "builds two independent Receiving installations and compares the unchanged overlay"]
async fn unchanged_overlay_across_baseline_and_additive_installations() -> anyhow::Result<()> {
    let evidence = super::evidence_directory().await?;
    super::with_signals(&evidence, async {
        fs::create_dir(&evidence)?;
        super::postcommit_case::run_selected(BaseCandidate::Baseline, &evidence.join("baseline"))
            .await?;
        super::postcommit_case::run_selected(BaseCandidate::Additive, &evidence.join("additive"))
            .await?;
        let (source, baseline) = installation(&evidence.join("baseline"), "baseline")?;
        let (additive_source, additive) = installation(&evidence.join("additive"), "additive")?;
        ensure!(
            source == additive_source,
            "the two installations must use the same source commit"
        );
        let result = compare(&source, &baseline, &additive)?;
        fs::write(
            evidence.join("pair.json"),
            serde_json::to_vec_pretty(&result)?,
        )?;
        Ok(())
    })
    .await
}

fn read(directory: &Path, name: &str) -> anyhow::Result<Value> {
    serde_json::from_slice(&fs::read(directory.join(name))?)
        .context("read the completed Receiving installation result")
}

fn installation(directory: &Path, base: &str) -> anyhow::Result<(String, Value)> {
    let verdict = read(directory, "verdict.json")?;
    let source = verdict["source"]
        .as_str()
        .context("the installation has its source commit")?;
    ensure!(
        source.len() == 40 && source.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "the installation source commit is invalid"
    );
    ensure!(
        verdict
            == json!({
                "schema":"wamn-receiving-postcommit/v1", "verdict":"pass", "source":source, "base":base,
                "failure":null,
                "passing_arms":["unchanged-overlay-routes","schema-requirements-observed",
                    "duplicate-handler-delivery","retry-exhaustion-advisory","independent-event-progress"],
            }),
        "the installation did not pass every retained case and owned cleanup"
    );
    let postcommit = read(directory, "postcommit.json")?;
    ensure!(
        postcommit["schema"] == "wamn-receiving-postcommit/v0.1"
            && postcommit["source"] == source
            && postcommit["verdict"] == "pass"
            && postcommit["cleanup"] == json!({"lock_released":true,"materializer_restored":true}),
        "the materializer result lacks successful resource restoration"
    );
    let document = read(directory, "overlay-compatibility.json")?;
    ensure!(
        document["case"] == "unchanged-overlay-compatibility"
            && document["base"] == base
            && document["stage"] == "installed-schema-observed",
        "the compatibility observation is incomplete"
    );
    let schema = &document["schema_observation"];
    ensure!(
        schema["result"] == "satisfied"
            && ["tables", "fields", "constraints"]
                .iter()
                .all(|key| schema[key].as_u64().is_some_and(|count| count > 0)),
        "the installation did not execute every schema observation"
    );
    let files = document["overlay_files"]
        .as_object()
        .context("the overlay has its file identities")?;
    ensure!(
        files.contains_key("wamn.json")
            && files.contains_key("generated/package-weld.json")
            && files.values().all(digest),
        "the immutable overlay file identities are incomplete"
    );
    ensure!(
        [
            "overlay_component_sha256",
            "base_initial_migration_sha256",
            "observed_schema_sha256"
        ]
        .iter()
        .all(|key| digest(&document[key])),
        "the installation has an invalid artifact identity"
    );
    ensure!(
        document["schema_admission_claim"] == false,
        "a schema observation cannot claim production admission enforcement"
    );
    Ok((source.to_owned(), document))
}

fn digest(value: &Value) -> bool {
    value
        .as_str()
        .and_then(|value| value.strip_prefix("sha256:"))
        .is_some_and(|hex| {
            hex.len() == 64
                && hex
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        })
}

fn compare(source: &str, baseline: &Value, additive: &Value) -> anyhow::Result<Value> {
    for key in [
        "overlay_files",
        "overlay_component_sha256",
        "required_schema_contract",
    ] {
        ensure!(
            baseline[key] == additive[key],
            "the overlay changed between installations: {key}"
        );
    }
    for key in ["base_initial_migration_sha256", "observed_schema_sha256"] {
        ensure!(
            baseline[key] != additive[key],
            "the additive candidate did not change {key}"
        );
    }
    let breaking = &baseline["breaking"];
    ensure!(
        breaking["cleanup"] == "database-absent" && breaking["overlay_unchanged"] == true,
        "the breaking installation lacks cleanup or unchanged overlay evidence"
    );
    ensure!(
        breaking["database"]
            .as_str()
            .and_then(|value| value.strip_prefix("receiving_overlay_break_"))
            .is_some_and(|suffix| suffix.len() == 32
                && suffix
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))),
        "the breaking candidate lacks a separate database identity"
    );
    ensure!(
        digest(&breaking["base_initial_migration_sha256"]),
        "the breaking candidate lacks its migration identity"
    );
    ensure!(
        breaking["refusal"]
            == json!({"code":"base-definition-mutation-refused","schema":"receiving",
        "relation":"purchase_order","definition":"acme_inspection_required","owner":"wamn_receiving","partial_overlay_state":false}),
        "the breaking installation did not produce the required ownership refusal"
    );
    Ok(
        json!({"case":"receiving-postcommit-pair","result":"pass","source_commit":source,
        "independent_installations":2,"overlay_file_count":baseline["overlay_files"].as_object().context("overlay files remain an object")?.len(),
        "overlay_component_sha256":baseline["overlay_component_sha256"],
        "baseline_observed_schema_sha256":baseline["observed_schema_sha256"],
        "additive_observed_schema_sha256":additive["observed_schema_sha256"],
        "breaking_refusal":breaking["refusal"],"cleanup":"pass"}),
    )
}

#[cfg(test)]
mod tests {
    use super::{compare, digest};
    use serde_json::{Value, json};

    fn installations() -> (Value, Value) {
        let hash = format!("sha256:{}", "a".repeat(64));
        let baseline = json!({"overlay_files":{"wamn.json":hash},"overlay_component_sha256":hash,
            "required_schema_contract":{"version":1},"base_initial_migration_sha256":"base-a","observed_schema_sha256":"schema-a",
            "breaking":{"cleanup":"database-absent","overlay_unchanged":true,
                "database":format!("receiving_overlay_break_{}", "a".repeat(32)),"base_initial_migration_sha256":hash,
                "refusal":{"code":"base-definition-mutation-refused","schema":"receiving","relation":"purchase_order",
                    "definition":"acme_inspection_required","owner":"wamn_receiving","partial_overlay_state":false}}});
        let mut additive = baseline.clone();
        additive["base_initial_migration_sha256"] = json!("base-b");
        additive["observed_schema_sha256"] = json!("schema-b");
        (baseline, additive)
    }

    #[test]
    fn requires_the_same_overlay_and_distinct_observed_base() {
        let (baseline, additive) = installations();
        assert!(compare("source", &baseline, &additive).is_ok());
        for key in [
            "overlay_files",
            "overlay_component_sha256",
            "required_schema_contract",
        ] {
            let mut changed = additive.clone();
            changed[key] = Value::Null;
            assert!(compare("source", &baseline, &changed).is_err());
        }
        for key in ["base_initial_migration_sha256", "observed_schema_sha256"] {
            let mut unchanged = additive.clone();
            unchanged[key] = baseline[key].clone();
            assert!(compare("source", &baseline, &unchanged).is_err());
        }
    }

    #[test]
    fn requires_the_exact_ownership_refusal_and_private_database_cleanup() {
        let (baseline, additive) = installations();
        for key in [
            "cleanup",
            "overlay_unchanged",
            "database",
            "base_initial_migration_sha256",
            "refusal",
        ] {
            let mut invalid = baseline.clone();
            invalid["breaking"][key] = Value::Null;
            assert!(compare("source", &invalid, &additive).is_err());
        }
        assert!(!digest(&json!(format!("sha256:{}", "A".repeat(64)))));
    }
}
