//! Observe selected SQL Jobs, retries, mode transitions, and matching adoption.
//!
//! Stale-event admission has no test because deployed environments have no
//! workflow authoring operation (wamn-alrvy.19).

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context as _, ensure};
use serde_json::Value;
use wamn_environment_control::environment::document::SchemaMode;

use super::{
    Journey, migration_records, mode, query, runtime, same_database, schema_command, state, stored,
    version,
};

// These observations are limited to the SQL Job and the installed cluster state.
pub(super) async fn exercise(
    journey: &Journey<'_>,
    root: &Path,
    database: &tokio_postgres::Client,
    rows: &mut BTreeMap<&str, (Value, Value)>,
    a0: &str,
    beta: &str,
) -> anyhow::Result<()> {
    eprintln!("scenario 3: SQL before code, then SQL after code through the deployed Job");
    mode(journey, SchemaMode::Migrate)?;
    version(root, "1.0.1", "1.1.0")?;
    std::fs::write(
        root.join("migrations/0002_counter_label.sql"),
        "CREATE INDEX counter_label_idx ON alpha_data.counter (label);\n",
    )?;
    let a2 = journey.publish("alpha", "1.1.0", root).await?;
    let identity = stored(journey, database).await?;
    let alpha = runtime(journey, "alpha").await?;
    let other = runtime(journey, "beta").await?;
    journey
        .apply(
            "diff-before-code",
            &mut schema_command(journey, "diff", Some(&a2)),
        )
        .await?;
    state(
        journey,
        database,
        &identity,
        rows,
        [(a0, "v1"), (beta, "v1")],
    )
    .await?;
    journey
        .apply(
            "sql-before-code",
            &mut schema_command(journey, "apply", Some(&a2)),
        )
        .await?;
    ensure!(query(database, "SELECT to_jsonb(i.indisvalid) FROM pg_index i WHERE i.indexrelid='alpha_data.counter_label_idx'::regclass").await? == true,
        "SQL Job did not install the selected index");
    ensure!(
        runtime(journey, "alpha").await? == alpha && runtime(journey, "beta").await? == other,
        "SQL-only apply changed an application workload or selection"
    );
    let installed = stored(journey, database).await?;
    same_database(&identity, &installed)?;
    ensure!(
        installed["fingerprint"] != identity["fingerprint"],
        "SQL Job did not advance schema identity"
    );
    state(
        journey,
        database,
        &installed,
        rows,
        [(a0, "v1"), (beta, "v1")],
    )
    .await?;
    journey
        .apply("code-after-sql", &mut journey.deploy(&a2))
        .await?;
    ensure!(
        runtime(journey, "beta").await? == other,
        "code after SQL changed beta"
    );
    state(
        journey,
        database,
        &installed,
        rows,
        [(&a2, "v1"), (beta, "v1")],
    )
    .await?;

    version(root, "1.1.0", "1.1.1")?;
    std::fs::write(
        root.join("migrations/0003_counter_data.sql"),
        "UPDATE alpha_data.counter SET label = label || '-migrated';\n",
    )?;
    let a3 = journey.publish("alpha", "1.1.1", root).await?;
    let alpha = runtime(journey, "alpha").await?;
    journey
        .apply(
            "diff-after-code",
            &mut schema_command(journey, "diff", Some(&a3)),
        )
        .await?;
    state(
        journey,
        database,
        &installed,
        rows,
        [(&a2, "v1"), (beta, "v1")],
    )
    .await?;
    journey
        .apply(
            "sql-after-code",
            &mut schema_command(journey, "apply", Some(&a3)),
        )
        .await?;
    let counter = &mut rows.get_mut("alpha").context("alpha row")?.0;
    counter["label"] = format!(
        "{}-migrated",
        counter["label"].as_str().context("counter label")?
    )
    .into();
    let migrated = stored(journey, database).await?;
    same_database(&identity, &migrated)?;
    let records = migration_records(database).await?;
    journey
        .apply(
            "repeat-sql",
            &mut schema_command(journey, "apply", Some(&a3)),
        )
        .await?;
    ensure!(
        migration_records(database).await? == records,
        "SQL retry changed committed checksums or outcomes"
    );
    ensure!(
        runtime(journey, "alpha").await? == alpha && runtime(journey, "beta").await? == other,
        "SQL after code changed serving applications"
    );
    state(
        journey,
        database,
        &migrated,
        rows,
        [(&a2, "v1"), (beta, "v1")],
    )
    .await?;

    eprintln!("scenario 4: mode transitions and matching adoption");
    // Select the installed schema source before matching adoption. Adoption itself
    // must preserve the running selection and the already executed data effect.
    journey
        .apply("select-installed-schema", &mut journey.deploy(&a3))
        .await?;
    let alpha = runtime(journey, "alpha").await?;
    for (iteration, selected) in [SchemaMode::Reset, SchemaMode::Migrate]
        .into_iter()
        .enumerate()
    {
        mode(journey, selected)?;
        journey
            .apply(
                &format!("mode-diff-{iteration}"),
                &mut schema_command(journey, "diff", None),
            )
            .await?;
        state(
            journey,
            database,
            &migrated,
            rows,
            [(&a3, "v1"), (beta, "v1")],
        )
        .await?;
        ensure!(
            runtime(journey, "alpha").await? == alpha && runtime(journey, "beta").await? == other,
            "configuration edit or diff changed an application workload"
        );
        if selected == SchemaMode::Migrate {
            journey
                .apply(
                    &format!("matching-adopt-{iteration}"),
                    journey
                        .command(&["schema", "adopt", "--env"])
                        .arg(&journey.file),
                )
                .await?;
            ensure!(
                migration_records(database).await? == records,
                "matching adoption changed execution checksums or outcomes"
            );
            state(
                journey,
                database,
                &migrated,
                rows,
                [(&a3, "v1"), (beta, "v1")],
            )
            .await?;
        }
    }
    Ok(())
}
