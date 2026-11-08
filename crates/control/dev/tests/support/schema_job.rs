//! Observe published SQL execution through the deployed Job and serving host.

use anyhow::ensure;
use serde_json::json;

use super::{Journey, observer, runtime, same_database, schema_command, stored};

pub(super) async fn exercise(
    journey: &Journey<'_>,
    starting: &str,
    selected_sql: &str,
) -> anyhow::Result<()> {
    journey
        .apply("install-selected-code", &mut journey.deploy(starting))
        .await?;
    let row = journey
        .route(
            "alpha",
            "counter/create",
            &json!([{
                "request_id": uuid::Uuid::new_v4().to_string(),
                "idempotency_key": uuid::Uuid::new_v4().to_string(),
                "label": "before"
            }]),
        )
        .await?;
    let database = observer(journey).await?;
    let before = stored(journey, &database).await?;
    let code = runtime(journey, "alpha").await?;
    journey
        .apply(
            "published-sql-job",
            &mut schema_command(journey, "apply", Some(selected_sql)),
        )
        .await?;
    ensure!(
        runtime(journey, "alpha").await? == code && code["current"] == starting,
        "SQL Job changed the selected or serving application"
    );
    let after = stored(journey, &database).await?;
    same_database(&before, &after)?;
    let found = journey
        .route("alpha", "counter/get", &json!([{"id": row["id"]}]))
        .await?;
    let mut expected = row;
    expected["label"] = "before-migrated".into();
    ensure!(
        found == expected,
        "SQL Job did not execute the selected published data migration"
    );
    Ok(())
}
