//! The same bounded command history at the model and deployed application boundaries.
use std::collections::BTreeMap;

use anyhow::Context as _;
use serde_json::{Value, json};
use tokio_postgres::Client;

use super::cases::History;
use super::oracle::{
    self, Action, Command, Disposition, Inventory, Lifecycle, Outcome, Packaging, PackagingType,
    State,
};

const TABLES: [&str; 6] = [
    "inventory_move",
    "inventory_adjust",
    "inventory_split",
    "inventory_merge",
    "packaging_relocate",
    "packaging_close",
];
const PRINCIPAL: &str = "00000000-0000-4000-8000-0000000000f1";

/// A business discrepancy, distinct from an unavailable test environment.
#[derive(Debug)]
pub(super) struct Mismatch {
    pub property: &'static str,
    pub operation: &'static str,
    pub expected: &'static str,
    pub observed: &'static str,
    pub detail: String,
}
impl std::fmt::Display for Mismatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} / {}: expected {}, observed {}: {}",
            self.operation, self.property, self.expected, self.observed, self.detail
        )
    }
}
impl std::error::Error for Mismatch {}

fn compare(
    property: &'static str,
    operation: &'static str,
    expected: &Value,
    observed: &Value,
) -> anyhow::Result<()> {
    if expected != observed {
        return Err(Mismatch {
            property,
            operation,
            expected: "equal",
            observed: "different",
            detail: format!("expected {expected}; observed {observed}"),
        }
        .into());
    }
    Ok(())
}
fn id(category: u8, value: bool) -> String {
    format!(
        "00000000-0000-0000-0000-000000000{category}0{}",
        u8::from(value) + 1
    )
}
fn lifecycle(value: Lifecycle) -> &'static str {
    match value {
        Lifecycle::Open => "open",
        Lifecycle::Closed => "closed",
    }
}
fn disposition(value: Disposition) -> &'static str {
    match value {
        Disposition::Available => "available",
        Disposition::Held => "held",
    }
}
fn packaging_type(value: PackagingType) -> &'static str {
    match value {
        PackagingType::Pallet => "pallet",
        PackagingType::Tote => "tote",
    }
}
fn reason(value: Option<bool>) -> Option<&'static str> {
    value.map(|v| if v { "damage-count" } else { "cycle-count" })
}
fn timestamp(value: bool) -> &'static str {
    if value {
        "2026-09-26T13:00:00Z"
    } else {
        "2026-09-26T12:00:00Z"
    }
}
fn operation(action: Action) -> &'static str {
    match action {
        Action::Move { .. } => TABLES[0],
        Action::Adjust { .. } => TABLES[1],
        Action::Split { .. } => TABLES[2],
        Action::Merge { .. } => TABLES[3],
        Action::RelocatePackaging { .. } => TABLES[4],
        Action::ClosePackaging { .. } => TABLES[5],
    }
}
fn route(action: Action) -> &'static str {
    match action {
        Action::Move { .. } => "/inventory/move",
        Action::Adjust { .. } => "/inventory/adjust",
        Action::Split { .. } => "/inventory/split",
        Action::Merge { .. } => "/inventory/merge",
        Action::RelocatePackaging { .. } => "/packaging/relocate",
        Action::ClosePackaging { .. } => "/packaging/close",
    }
}

#[derive(Debug)]
struct Bindings {
    inventory: [String; 2],
    operations: [Option<String>; 2],
    transactions: BTreeMap<u8, String>,
}
impl Default for Bindings {
    fn default() -> Self {
        Self {
            inventory: [id(3, false), id(3, true)],
            operations: [None, None],
            transactions: BTreeMap::new(),
        }
    }
}
#[derive(Debug)]
struct Accepted {
    command: Command,
    body: Value,
    result: Value,
}

async fn seed(db: &Client, state: State) -> anyhow::Result<()> {
    anyhow::ensure!(
        state.operations == [None, None],
        "history fixture starts without claims"
    );
    let commands = TABLES
        .iter()
        .map(|table| format!("wms.{table}_command"))
        .collect::<Vec<_>>()
        .join(", ");
    db.batch_execute(&format!("BEGIN; SELECT set_config('app.user_id', '{PRINCIPAL}', true), set_config('app.operation', 'admin:differential-fixture', true); INSERT INTO app_system.users (tenant_id,id,type,email) VALUES ('{}','{PRINCIPAL}','person','differential@example.invalid') ON CONFLICT DO NOTHING; TRUNCATE {commands}, wms.packaging_create_command, wms.product_command, wms.location_command, wms.inventory_transaction, wms.inventory, wms.packaging, wms.product, wms.location;", crate::environment::TENANT)).await?;
    for value in [false, true] {
        db.execute(
            "INSERT INTO wms.product(id,product_code) VALUES($1::text::uuid,$2)",
            &[&id(1, value), &format!("PRODUCT-{value}")],
        )
        .await?;
        db.execute(
            "INSERT INTO wms.location(id,location_code) VALUES($1::text::uuid,$2)",
            &[&id(2, value), &format!("LOCATION-{value}")],
        )
        .await?;
    }
    for p in state.packaging {
        db.execute("INSERT INTO wms.packaging(id,type,code,location_id,lifecycle) VALUES($1::text::uuid,$2,$3,$4::text::uuid,$5)", &[&id(5,p.id), &packaging_type(p.r#type), &format!("PACKAGING-{}", p.code), &id(2,p.location_id), &lifecycle(p.lifecycle)]).await?;
    }
    for i in state.inventory.into_iter().flatten() {
        db.execute("INSERT INTO wms.inventory(id,product_id,packaging_id,location_id,quantity,disposition,lifecycle) VALUES($1::text::uuid,$2::text::uuid,$3::text::uuid,$4::text::uuid,$5::text::numeric,$6,$7)", &[&id(3,i.id), &id(1,i.product_id), &id(5,i.packaging_id), &id(2,i.location_id), &i.quantity.to_string(), &disposition(i.disposition), &lifecycle(i.lifecycle)]).await?;
    }
    db.batch_execute("COMMIT").await?;
    Ok(())
}

async fn revision(db: &Client, table: &str, id: &str) -> anyhow::Result<i32> {
    Ok(db
        .query_opt(
            &format!("SELECT row_version FROM wms.{table} WHERE id=$1::text::uuid"),
            &[&id],
        )
        .await?
        .map_or(1, |r| r.get(0)))
}
async fn body(
    db: &Client,
    command: Command,
    bindings: &Bindings,
    prior: Option<&Accepted>,
) -> anyhow::Result<Value> {
    if let Some(prior) = prior.filter(|p| p.command == command) {
        return Ok(prior.body.clone());
    }
    let mut v = json!({"idempotency_key":format!("differential-{}", command.key), "occurred_at":timestamp(command.occurred_at)});
    let source = match command.action {
        Action::Move {
            inventory_id,
            to_packaging_id,
            to_location_id,
        } => {
            v["inventory_id"] = json!(bindings.inventory[usize::from(inventory_id)]);
            v["to_packaging_id"] = json!(id(5, to_packaging_id));
            v["to_location_id"] = json!(id(2, to_location_id));
            Some(inventory_id)
        }
        Action::Adjust {
            inventory_id,
            to_quantity,
        } => {
            v["inventory_id"] = json!(bindings.inventory[usize::from(inventory_id)]);
            v["to_quantity"] = json!(to_quantity.to_string());
            v["reason"] = json!(reason(command.reason).unwrap_or(""));
            Some(inventory_id)
        }
        Action::Split {
            from_inventory_id,
            quantity,
            to_packaging_id,
            to_location_id,
        } => {
            v["from_inventory_id"] = json!(bindings.inventory[usize::from(from_inventory_id)]);
            v["quantity"] = json!(quantity.to_string());
            v["to_packaging_id"] = json!(id(5, to_packaging_id));
            v["to_location_id"] = json!(id(2, to_location_id));
            Some(from_inventory_id)
        }
        Action::Merge {
            from_inventory_id,
            to_inventory_id,
        } => {
            v["from_inventory_id"] = json!(bindings.inventory[usize::from(from_inventory_id)]);
            v["to_inventory_id"] = json!(bindings.inventory[usize::from(to_inventory_id)]);
            v["expected_from_row_version"] = json!(
                revision(
                    db,
                    "inventory",
                    &bindings.inventory[usize::from(from_inventory_id)]
                )
                .await?
            );
            v["expected_to_row_version"] = json!(
                revision(
                    db,
                    "inventory",
                    &bindings.inventory[usize::from(to_inventory_id)]
                )
                .await?
            );
            None
        }
        Action::RelocatePackaging {
            packaging_id,
            to_location_id,
        } => {
            v["packaging_id"] = json!(id(5, packaging_id));
            v["to_location_id"] = json!(id(2, to_location_id));
            v["expected_row_version"] =
                json!(revision(db, "packaging", &id(5, packaging_id)).await?);
            None
        }
        Action::ClosePackaging { packaging_id } => {
            v.as_object_mut().unwrap().remove("occurred_at");
            v["packaging_id"] = json!(id(5, packaging_id));
            v["expected_row_version"] =
                json!(revision(db, "packaging", &id(5, packaging_id)).await?);
            None
        }
    };
    if let Some(source) = source {
        v["expected_row_version"] =
            json!(revision(db, "inventory", &bindings.inventory[usize::from(source)]).await?);
    }
    if let Some(prior) = prior {
        anyhow::ensure!(
            operation(prior.command.action) == operation(command.action),
            "each model key belongs to one production command namespace"
        );
        for field in [
            "expected_row_version",
            "expected_from_row_version",
            "expected_to_row_version",
        ] {
            if let Some(value) = prior.body.get(field) {
                v[field] = value.clone();
            }
        }
    }
    Ok(v)
}

async fn post(
    http: &reqwest::Client,
    endpoint: &str,
    host: &str,
    bearer: &str,
    action: Action,
    body: &Value,
) -> anyhow::Result<Value> {
    let response = http
        .post(format!("{endpoint}{}", route(action)))
        .header("Host", host)
        .bearer_auth(bearer)
        .json(&json!([{ "request_id":"differential", "value":body }]))
        .send()
        .await?;
    let status = response.status();
    let text = response.text().await?;
    if status == reqwest::StatusCode::BAD_REQUEST {
        let refusal: Value = serde_json::from_str(&text)?;
        if refusal["error"]["code"] == "schema-invalid" {
            // Wire validation can refuse a modeled invalid command before dispatch.
            return Ok(refusal);
        }
    }
    anyhow::ensure!(status.is_success(), "differential HTTP {status}: {text}");
    let answer: Value = serde_json::from_str(&text)?;
    let items = answer
        .as_array()
        .context("application response is an array")?;
    anyhow::ensure!(
        items.len() == 1 && items[0]["request_id"] == "differential",
        "application returned invalid envelope: {answer}"
    );
    if let Some(error) = items[0].get("error") {
        anyhow::ensure!(
            matches!(
                error["code"].as_str(),
                Some(
                    "invalid_input"
                        | "not_found"
                        | "idempotency_conflict"
                        | "insufficient_quantity"
                )
            ),
            "non-business application refusal: {error}"
        );
    }
    Ok(items[0].clone())
}

fn inventory(i: Inventory, bindings: &Bindings) -> Value {
    json!({"id":bindings.inventory[usize::from(i.id)], "product_id":id(1,i.product_id), "packaging_id":id(5,i.packaging_id), "location_id":id(2,i.location_id), "quantity":i.quantity.to_string(), "disposition":disposition(i.disposition), "lifecycle":lifecycle(i.lifecycle)})
}
fn packaging(p: Packaging) -> Value {
    json!({"id":id(5,p.id),"type":packaging_type(p.r#type),"code":format!("PACKAGING-{}",p.code),"location_id":id(2,p.location_id),"lifecycle":lifecycle(p.lifecycle)})
}
fn sorted(mut rows: Vec<Value>, field: &str) -> Value {
    rows.sort_by(|a, b| a[field].as_str().cmp(&b[field].as_str()));
    json!(rows)
}

async fn raw_snapshot(db: &Client) -> anyhow::Result<Value> {
    let mut result = json!({});
    for table in ["inventory", "packaging", "inventory_transaction"] {
        let rows = db
            .query(
                &format!("SELECT to_jsonb(t) FROM wms.{table} t ORDER BY id"),
                &[],
            )
            .await?;
        result[table] = json!(
            rows.iter()
                .map(|r| r.get::<_, Value>(0))
                .collect::<Vec<_>>()
        );
    }
    for table in TABLES {
        let rows = db
            .query(
                &format!("SELECT to_jsonb(t) FROM wms.{table}_command t ORDER BY idempotency_key"),
                &[],
            )
            .await?;
        result[table] = json!(
            rows.iter()
                .map(|r| r.get::<_, Value>(0))
                .collect::<Vec<_>>()
        );
    }
    Ok(result)
}

fn result_projection(
    command: Command,
    result: oracle::CommandResult,
    bindings: &Bindings,
) -> Value {
    let mut value = match command.action {
        Action::Move { inventory_id, .. } | Action::Adjust { inventory_id, .. } => inventory(
            result.inventory[usize::from(inventory_id)].unwrap(),
            bindings,
        ),
        Action::Split {
            from_inventory_id, ..
        } => {
            let mut v = inventory(
                result.inventory[usize::from(from_inventory_id)].unwrap(),
                bindings,
            );
            v["new_inventory_id"] = json!(bindings.inventory[usize::from(!from_inventory_id)]);
            v
        }
        Action::Merge {
            from_inventory_id,
            to_inventory_id,
        } => {
            let mut v = inventory(
                result.inventory[usize::from(to_inventory_id)].unwrap(),
                bindings,
            );
            v["from_inventory_id"] = json!(bindings.inventory[usize::from(from_inventory_id)]);
            v
        }
        Action::RelocatePackaging { packaging_id, .. }
        | Action::ClosePackaging { packaging_id } => {
            packaging(result.packaging[usize::from(packaging_id)])
        }
    };
    let key = if matches!(
        command.action,
        Action::ClosePackaging { .. } | Action::RelocatePackaging { .. }
    ) {
        "packaging_id"
    } else {
        "inventory_id"
    };
    let identity = value.as_object_mut().unwrap().remove("id").unwrap();
    value[key] = identity;
    value["operation_id"] = json!(bindings.operations[usize::from(result.operation_id)]);
    value
}

async fn assert_state(
    db: &Client,
    state: State,
    bindings: &mut Bindings,
    op: &'static str,
) -> anyhow::Result<()> {
    let rows = db.query("SELECT jsonb_build_object('id',id,'product_id',product_id,'packaging_id',packaging_id,'location_id',location_id,'quantity',trim_scale(quantity)::text,'disposition',disposition,'lifecycle',lifecycle) FROM wms.inventory ORDER BY id", &[]).await?;
    compare(
        "inventory",
        op,
        &sorted(
            state
                .inventory
                .into_iter()
                .flatten()
                .map(|i| inventory(i, bindings))
                .collect(),
            "id",
        ),
        &json!(
            rows.iter()
                .map(|r| r.get::<_, Value>(0))
                .collect::<Vec<_>>()
        ),
    )?;
    let rows = db.query("SELECT to_jsonb(p)-'row_version'-'created_at'-'created_by'-'updated_at'-'updated_by' FROM wms.packaging p ORDER BY id", &[]).await?;
    // Select explicit business fields so platform audit additions remain outside the oracle.
    let observed = rows.iter().map(|r| { let v: Value = r.get(0); json!({"id":v["id"],"type":v["type"],"code":v["code"],"location_id":v["location_id"],"lifecycle":v["lifecycle"]}) }).collect::<Vec<_>>();
    compare(
        "packaging",
        op,
        &sorted(state.packaging.into_iter().map(packaging).collect(), "id"),
        &json!(observed),
    )?;
    let rows = db.query("SELECT to_jsonb(t) || jsonb_build_object('from_quantity',trim_scale(from_quantity)::text,'to_quantity',trim_scale(to_quantity)::text,'occurred_at',to_char(occurred_at AT TIME ZONE 'UTC','YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"')) FROM wms.inventory_transaction t ORDER BY id", &[]).await?;
    let observed = rows
        .iter()
        .map(|r| r.get::<_, Value>(0))
        .collect::<Vec<_>>();
    let mut expected = Vec::new();
    for t in state
        .operations
        .into_iter()
        .flatten()
        .flat_map(|o| o.transactions.into_iter().flatten())
    {
        let operation_id = bindings.operations[usize::from(t.operation_id)]
            .as_ref()
            .context("accepted operation has a production identity")?;
        let inventory_id = &bindings.inventory[usize::from(t.inventory_id)];
        let matching = observed
            .iter()
            .filter(|v| v["operation_id"] == *operation_id && v["inventory_id"] == *inventory_id)
            .collect::<Vec<_>>();
        if matching.len() != 1 {
            return Err(Mismatch {
                property: "history membership",
                operation: op,
                expected: "one row per affected identity",
                observed: "different",
                detail: format!("model row {t:?}; database {observed:?}"),
            }
            .into());
        }
        let row_id = matching[0]["id"]
            .as_str()
            .context("history UUID is text")?
            .to_owned();
        let bound = bindings.transactions.entry(t.id).or_insert(row_id);
        let r#type = match t.r#type {
            oracle::Type::Move => "move",
            oracle::Type::Adjust => "adjust",
            oracle::Type::Split => "split",
            oracle::Type::Merge => "merge",
            oracle::Type::RelocatePackaging => "packaging_relocate",
        };
        expected.push(json!({"id":bound,"operation_id":operation_id,"type":r#type,"inventory_id":inventory_id,"from_inventory_id":bindings.inventory[usize::from(t.from_inventory_id)],"to_inventory_id":bindings.inventory[usize::from(t.to_inventory_id)],"from_product_id":t.from_product_id.map(|v|id(1,v)),"to_product_id":id(1,t.to_product_id),"from_packaging_id":t.from_packaging_id.map(|v|id(5,v)),"to_packaging_id":id(5,t.to_packaging_id),"from_location_id":t.from_location_id.map(|v|id(2,v)),"to_location_id":id(2,t.to_location_id),"from_quantity":t.from_quantity.to_string(),"to_quantity":t.to_quantity.to_string(),"from_disposition":t.from_disposition.map(disposition),"to_disposition":disposition(t.to_disposition),"from_lifecycle":t.from_lifecycle.map(lifecycle),"to_lifecycle":lifecycle(t.to_lifecycle),"occurred_at":timestamp(t.occurred_at),"reason":reason(t.reason)}));
    }
    let fields = [
        "id",
        "operation_id",
        "type",
        "inventory_id",
        "from_inventory_id",
        "to_inventory_id",
        "from_product_id",
        "to_product_id",
        "from_packaging_id",
        "to_packaging_id",
        "from_location_id",
        "to_location_id",
        "from_quantity",
        "to_quantity",
        "from_disposition",
        "to_disposition",
        "from_lifecycle",
        "to_lifecycle",
        "occurred_at",
        "reason",
    ];
    let projected = observed
        .into_iter()
        .map(|row| {
            let mut v = json!({});
            for field in fields {
                v[field] = row[field].clone();
            }
            v
        })
        .collect();
    compare(
        "immutable history",
        op,
        &sorted(expected, "id"),
        &sorted(projected, "id"),
    )
}

pub(super) async fn run_history(
    db: &Client,
    endpoint: &str,
    host: &str,
    bearer: &str,
    history: &History,
) -> anyhow::Result<()> {
    seed(db, history.initial).await?;
    let http = reqwest::Client::new();
    let mut state = history.initial;
    let mut bindings = Bindings::default();
    let mut accepted: Vec<Accepted> = Vec::new();
    let mut previous = raw_snapshot(db).await?;
    for (step, command) in history.commands.iter().copied().enumerate() {
        let op = operation(command.action);
        let prior = accepted.iter().find(|p| p.command.key == command.key);
        let request = body(db, command, &bindings, prior).await?;
        let outcome = oracle::execute(&mut state, command);
        let response = post(&http, endpoint, host, bearer, command.action, &request).await?;
        let succeeded = response.get("value").is_some();
        let expected_success = matches!(outcome, Outcome::Accepted(_) | Outcome::Replayed(_));
        if succeeded != expected_success {
            return Err(Mismatch {
                property: "outcome",
                operation: op,
                expected: if expected_success {
                    "accepted"
                } else {
                    "refused"
                },
                observed: if succeeded { "accepted" } else { "refused" },
                detail: format!("step {step}: {command:?}; model {outcome:?}; response {response}"),
            }
            .into());
        }
        match outcome {
            Outcome::Accepted(result) => {
                let value = &response["value"];
                let actual = value["operation_id"]
                    .as_str()
                    .context("accepted result has an operation UUID")?
                    .to_owned();
                compare(
                    "operation identity is new",
                    op,
                    &json!(false),
                    &json!(bindings.operations.iter().flatten().any(|id| id == &actual)),
                )?;
                uuid::Uuid::parse_str(&actual).context("operation identity is a UUID")?;
                bindings.operations[usize::from(result.operation_id)] = Some(actual);
                if let Action::Split {
                    from_inventory_id, ..
                } = command.action
                {
                    let new_id = value["new_inventory_id"]
                        .as_str()
                        .context("split returns its generated inventory UUID")?
                        .to_owned();
                    compare(
                        "split identity is new",
                        op,
                        &json!(false),
                        &json!(bindings.inventory.contains(&new_id)),
                    )?;
                    uuid::Uuid::parse_str(&new_id).context("split identity is a UUID")?;
                    bindings.inventory[usize::from(!from_inventory_id)] = new_id;
                }
                accepted.push(Accepted {
                    command,
                    body: request,
                    result: value.clone(),
                });
            }
            Outcome::Replayed(_) => compare(
                "exact replay",
                op,
                &prior
                    .context("model replay has an earlier accepted response")?
                    .result,
                &response["value"],
            )?,
            Outcome::Refused(_) => compare(
                "refusal preservation",
                op,
                &previous,
                &raw_snapshot(db).await?,
            )?,
            Outcome::Aborted => {
                anyhow::bail!("failure injection does not belong to this differential history")
            }
        }
        assert_state(db, state, &mut bindings, op).await?;
        let snapshot = raw_snapshot(db).await?;
        for row in previous["inventory_transaction"].as_array().unwrap() {
            let current = snapshot["inventory_transaction"]
                .as_array()
                .unwrap()
                .iter()
                .find(|v| v["id"] == row["id"])
                .unwrap_or(&Value::Null);
            compare("history prefix", op, row, current)?;
        }
        for table in TABLES {
            let expected_claims = accepted
                .iter()
                .filter(|a| operation(a.command.action) == table)
                .count();
            compare(
                "claim count",
                op,
                &json!(expected_claims),
                &json!(snapshot[table].as_array().unwrap().len()),
            )?;
            for claim in previous[table].as_array().unwrap() {
                let current = snapshot[table]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|v| v["idempotency_key"] == claim["idempotency_key"])
                    .unwrap_or(&Value::Null);
                compare("stored claim preservation", op, claim, current)?;
            }
        }
        for prior in &accepted {
            let model = state
                .operations
                .iter()
                .flatten()
                .find(|o| o.command == prior.command)
                .context("accepted result is retained by the oracle")?;
            let expected = result_projection(prior.command, model.result, &bindings);
            let mut observed = prior.result.clone();
            observed
                .as_object_mut()
                .context("operation result is an object")?
                .remove("row_version");
            compare("stored model result", op, &expected, &observed)?;
            let table = operation(prior.command.action);
            let claim = snapshot[table]
                .as_array()
                .unwrap()
                .iter()
                .find(|v| v["idempotency_key"] == format!("differential-{}", prior.command.key))
                .context("accepted command has a persisted claim")?;
            compare(
                "claim operation identity",
                op,
                &prior.result["operation_id"],
                &claim["operation_id"],
            )?;
            if matches!(prior.command.action, Action::Split { .. }) {
                compare(
                    "claim split identity",
                    op,
                    &prior.result["new_inventory_id"],
                    &claim["new_inventory_id"],
                )?;
            }
            let stored: Value = serde_json::from_str(
                claim["result"]
                    .as_str()
                    .context("claim stores its complete result")?,
            )?;
            compare("stored original response", op, &prior.result, &stored)?;
            let replay = post(
                &http,
                endpoint,
                host,
                bearer,
                prior.command.action,
                &prior.body,
            )
            .await?;
            compare("later exact replay", op, &prior.result, &replay["value"])?;
        }
        compare(
            "replay preservation",
            op,
            &snapshot,
            &raw_snapshot(db).await?,
        )?;
        previous = snapshot;
    }
    Ok(())
}
