//! Shared fixture and its local tests.

use sqlx::Row as _;
include!("overlay_compatibility_fixture.rs");

/// An additive base change leaves the overlay unchanged and compatible
/// (wamn-7k2fh, the additive overlay pair that b8199fb4c deleted): a base whose
/// initial migration adds a nullable field to `purchase_order`, rebuilt in a
/// test-owned workspace, installs fresh with the overlay as built, and the
/// overlay's required schema is satisfied exactly as on the unchanged base.
#[sqlx::test(migrations = false)]
#[ignore = "requires: bin:cargo, bin:rustc, input:client-acme"]
async fn an_additive_base_change_leaves_the_overlay_unchanged_and_compatible(
    pool: sqlx::PgPool,
) -> anyhow::Result<()> {
    // The nullable field the additive base adds.
    const FIELD: &str = "overlay_compatibility_note";
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
    // Apply preflights every mutation before it runs DDL, so the change is to
    // the fresh CREATE, not an ALTER of a relation absent at preflight.
    std::fs::write(
        &migration,
        text.replacen(header, &format!("{header}    {FIELD} text,\n"), 1),
    )?;
    let build_server = wamn_test_postgres::url(&pool);
    super::package_copies::build(&copies, &["wamn_receiving"], build_server.as_str()).await?;

    let required = &required_schema_contract(&overlay)?;
    let mut postgres = production_server().await?;
    let mut observations = Vec::new();
    for (name, base) in [
        ("baseline", super::package_root()),
        ("additive", additive_base),
    ] {
        let (_, mut project) = install_fresh(&mut postgres, name, &[base, overlay.clone()])
            .await
            .with_context(|| format!("install the unchanged overlay over the {name} base"))?;
        let observed = observe_installed_contract(&mut project, required).await;
        drop(project);

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
