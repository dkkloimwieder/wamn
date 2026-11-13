//! Shared fixture and its local tests.

include!("overlay_compatibility_fixture.rs");
#[test]
fn required_contract_observation_refuses_changed_consumed_fields_and_constraints() {
    let required = json!({"tables":[{"schema":"receiving","table":"odd_probe",
        "fields":[{"name":"probe_key","type":"uuid","nullable":false}],
        "constraints":[{"name":"odd_probe_key","definition":{"type":"primary_key","columns":["probe_key"]}}]}]});
    let live = json!({"tables":[{"schema":"receiving","name":"odd_probe",
        "columns":[{"name":"probe_key","type":"uuid","nullable":false},{"name":"unused","type":"text","nullable":true}],
        "constraints":[{"name":"odd_probe_key","type":"primary_key","columns":["probe_key"]}]}]});
    assert_eq!(
        required_contract_observation(&required, &live).unwrap(),
        json!({"tables":1,"fields":1,"constraints":1,"result":"satisfied"})
    );
    for changed in [json!("text"), json!("int64")] {
        let mut candidate = live.clone();
        candidate["tables"][0]["columns"][0]["type"] = changed;
        let error = required_contract_observation(&required, &candidate).unwrap_err();
        assert!(error.to_string().contains("receiving.odd_probe.probe_key"));
    }
    let mut nullable = live.clone();
    nullable["tables"][0]["columns"][0]["nullable"] = json!(true);
    assert!(
        required_contract_observation(&required, &nullable)
            .unwrap_err()
            .to_string()
            .contains("receiving.odd_probe.probe_key")
    );
    let mut constraint = live;
    constraint["tables"][0]["constraints"][0]["columns"] = json!(["unused"]);
    assert!(
        required_contract_observation(&required, &constraint)
            .unwrap_err()
            .to_string()
            .contains("receiving.odd_probe.odd_probe_key")
    );
}

/// Narrow seam test; the paired journey remains the fresh-install test.
#[tokio::test]
async fn installed_contract_observer_reads_a_production_install_and_refuses_changed_requirements()
-> anyhow::Result<()> {
    let mut postgres = production_server().await?;
    let (_, project, connection_task) = install_fresh(
        &mut postgres,
        "observer",
        &[super::package_root(), super::overlay_package_root()],
    )
    .await?;
    let version: i32 = project
        .query_one("SELECT current_setting('server_version_num')::integer", &[])
        .await?
        .get(0);
    let schema_acl = project
        .query_one(
            "SELECT nspacl::text FROM pg_namespace WHERE nspname = 'receiving'",
            &[],
        )
        .await?
        .get::<_, Option<String>>(0)
        .context("production install must retain an explicit schema ACL")?;
    let required = &required_schema_contract(&super::overlay_package_root())?;
    let baseline = observe_installed_contract(&project, required).await?;
    let positive = required_contract_observation(required, &baseline)?;
    project
        .batch_execute(
            "ALTER TABLE receiving.purchase_order ADD COLUMN overlay_compatibility_note text",
        )
        .await?;
    let additive = observe_installed_contract(&project, required).await?;
    ensure!(
        baseline != additive,
        "all-column projection omitted the additive field"
    );
    ensure!(
        required_contract_observation(required, &additive)? == positive,
        "additive observation changed satisfied requirements"
    );
    let controls = [
        (
            "consumed-nullability",
            "ALTER TABLE receiving.purchase_order ALTER COLUMN status DROP NOT NULL",
            "receiving.purchase_order.status",
        ),
        (
            "consumed-constraint",
            "ALTER TABLE receiving.quality_inspection DROP CONSTRAINT quality_inspection_status_check; ALTER TABLE receiving.quality_inspection ADD CONSTRAINT quality_inspection_status_check CHECK (status IN ('pending', 'approved', 'rejected'))",
            "receiving.quality_inspection.quality_inspection_status_check",
        ),
        (
            "unvalidated-constraint",
            "ALTER TABLE receiving.quality_inspection DROP CONSTRAINT quality_inspection_status_check; ALTER TABLE receiving.quality_inspection ADD CONSTRAINT quality_inspection_status_check CHECK (status IN ('pending', 'approved')) NOT VALID",
            "receiving.quality_inspection.quality_inspection_status_check",
        ),
        (
            "deferrable-foreign-key",
            "ALTER TABLE receiving.quality_inspection ALTER CONSTRAINT quality_inspection_receipt_id_fkey DEFERRABLE",
            "receiving.quality_inspection.quality_inspection_receipt_id_fkey",
        ),
    ];
    let mut negative = Vec::new();
    for (name, sql, expected) in controls {
        project.batch_execute("BEGIN").await?;
        project.batch_execute(sql).await?;
        let outcome = match observe_installed_contract(&project, required).await {
            Ok(observed) => required_contract_observation(required, &observed),
            Err(error) => Err(error),
        };
        project.batch_execute("ROLLBACK").await?;
        let error = outcome
            .err()
            .with_context(|| format!("observer accepted {name}"))?;
        ensure!(
            error.to_string().contains(expected),
            "{name} refused for a different reason: {error}"
        );
        negative.push(json!({"control":name,"refusal":error.to_string(),"result":"refused"}));
    }
    let restored = observe_installed_contract(&project, required).await?;
    ensure!(
        restored == additive,
        "rolled-back controls changed the installed projection"
    );
    let retained_acl: String = project
        .query_one(
            "SELECT nspacl::text FROM pg_namespace WHERE nspname = 'receiving'",
            &[],
        )
        .await?
        .get(0);
    ensure!(
        retained_acl == schema_acl,
        "observer changed installed ACLs"
    );
    let evidence = json!({
        "case":"installed-overlay-contract-observer", "server_version_num":version,
        "installed_schema_acl":schema_acl, "retained_schema_acl":retained_acl,
        "schema_admission_assertion":false,
        "positive":positive, "controls":negative,
        "baseline_observed_schema_sha256":bytes_sha256(&serde_json::to_vec(&baseline)?),
        "additive_observed_schema_sha256":bytes_sha256(&serde_json::to_vec(&additive)?),
        "result":"pass",
    });
    println!("{}", serde_json::to_string_pretty(&evidence)?);
    drop(project);
    connection_task.await?;
    Ok(())
}

/// An additive base change leaves the overlay unchanged and compatible
/// (wamn-7k2fh, the additive overlay pair that b8199fb4c deleted): a base whose
/// initial migration adds a nullable field to `purchase_order`, rebuilt in a
/// test-owned workspace, installs fresh with the overlay as built, and the
/// overlay's required schema is satisfied exactly as on the unchanged base.
#[tokio::test(flavor = "current_thread")]
#[ignore = "requires: cargo, rustc"]
async fn an_additive_base_change_leaves_the_overlay_unchanged_and_compatible() -> anyhow::Result<()>
{
    // The nullable field the additive base adds.
    const FIELD: &str = "overlay_compatibility_note";
    wamn_test_postgres::require_prerequisites(&["cargo", "rustc"]);
    let overlay = super::overlay_package_root();
    let overlay_before =
        wamn_test_infrastructure::fixture_digest::fixture_artifact_digest(&overlay)?;
    let scratch = super::ScratchRoot::create()?;
    let copies = scratch.path().join("copies");
    super::package_copies::create(&copies, &[super::package_root()])?;
    let additive_base = super::package_copies::package(&copies, "wamn_receiving");
    let migration = additive_base.join("migrations/0001_initial.sql");
    let header = "CREATE TABLE receiving.purchase_order (\n";
    let text = std::fs::read_to_string(&migration)?;
    ensure!(
        text.matches(header).count() == 1,
        "the initial migration declares purchase_order once"
    );
    // Apply preflights every mutation before it runs DDL, so the change is to
    // the fresh CREATE, not an ALTER of a relation absent at preflight.
    std::fs::write(
        &migration,
        text.replacen(header, &format!("{header}    {FIELD} text,\n"), 1),
    )?;
    let build_server = wamn_test_postgres::database();
    super::package_copies::build(&copies, &["wamn_receiving"], build_server.url()).await?;

    let required = &required_schema_contract(&overlay)?;
    let mut postgres = production_server().await?;
    let mut observations = Vec::new();
    for (name, base) in [
        ("baseline", super::package_root()),
        ("additive", additive_base),
    ] {
        let (_, project, connection_task) =
            install_fresh(&mut postgres, name, &[base, overlay.clone()])
                .await
                .with_context(|| format!("install the unchanged overlay over the {name} base"))?;
        let observed = observe_installed_contract(&project, required).await;
        drop(project);
        connection_task.await?;
        observations.push(observed?);
    }
    let [baseline, additive] = observations.as_slice() else {
        unreachable!("two installs were observed");
    };
    let field = |observed: &Value| -> anyhow::Result<Option<Value>> {
        Ok(observed["tables"]
            .as_array()
            .context("observed tables are an array")?
            .iter()
            .find(|table| table["schema"] == "receiving" && table["name"] == "purchase_order")
            .context("the install holds purchase_order")?["columns"]
            .as_array()
            .context("observed columns are an array")?
            .iter()
            .find(|column| column["name"] == FIELD)
            .cloned())
    };
    ensure!(
        field(baseline)?.is_none(),
        "the baseline holds the additive field"
    );
    ensure!(
        field(additive)?
            .is_some_and(|column| column["type"] == "text" && column["nullable"] == true),
        "the additive install lacks its nullable text field"
    );
    let satisfied = required_contract_observation(required, baseline)?;
    ensure!(
        required_contract_observation(required, additive)? == satisfied,
        "the additive base changed the overlay's satisfied requirements"
    );
    ensure!(
        wamn_test_infrastructure::fixture_digest::fixture_artifact_digest(&overlay)?
            == overlay_before,
        "the overlay package changed during the additive install"
    );
    println!("ADDITIVE_BASE_OVERLAY result=pass requirements={satisfied}");
    Ok(())
}
