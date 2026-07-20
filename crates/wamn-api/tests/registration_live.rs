//! Live-apply gate for the event-registration CRUD surface (EVT-REG, D19 v3 §5).
//!
//! Set `WAMN_API_PG_URL` to a **superuser** url (path `/postgres`) of a
//! throwaway Postgres (recipe: docs/build-and-test.md [EVT-REG]); skipped
//! cleanly when unset. Hermetic preamble: drops+recreates the `catalog` schema
//! and applies the REAL storage SQL (deploy/sql/catalog-schema.sql), then drives
//! the wamn-api registration builders through create → list → get → update →
//! delete **as the least-privilege `wamn_app` role under a tenant claim** — the
//! path the serving component runs — proving the SQL executes, the document
//! round-trips, and the RLS policy isolates tenants (a second tenant sees none
//! of the first's rows). Also proves `wamn_event_reg::validate` gates the write.

use tokio_postgres::types::ToSql;
use tokio_postgres::{Client, NoTls};

use wamn_api::SqlValue;
use wamn_api::registration;
use wamn_api::router::Compiled;
use wamn_catalog::Catalog;
use wamn_event_reg::{EventRegistration, Op, RegistrationState, SCHEMA_VERSION, validate};

const CATALOG_SCHEMA: &str = include_str!("../../../deploy/sql/catalog-schema.sql");

/// The catalog the registration binds to. Entity id `sales_orders` ≠ table name
/// `orders`, so a stored `entity_id` proves the id (rename-proof key) was used.
const CATALOG: &str = r#"{
  "schema-version": "0.1",
  "catalog-id": "shop",
  "version": 1,
  "entities": [
    { "id": "sales_orders", "name": "orders",
      "fields": [ { "id": "status", "name": "status", "type": { "kind": "text" } } ] },
    { "id": "line_items", "name": "lines",
      "fields": [ { "id": "qty", "name": "qty", "type": { "kind": "int" } } ] }
  ]
}"#;

fn superuser_url() -> Option<String> {
    std::env::var("WAMN_API_PG_URL").ok()
}

fn catalog() -> Catalog {
    Catalog::from_json(CATALOG).expect("catalog fixture parses")
}

fn reg(reg_id: &str, entity: &str) -> EventRegistration {
    EventRegistration {
        schema_version: SCHEMA_VERSION.to_string(),
        registration_id: reg_id.to_string(),
        catalog_id: "shop".to_string(),
        flow_id: "notify".to_string(),
        entity: entity.into(),
        ops: vec![Op::Insert, Op::Update],
        condition: Some("new.status == 'shipped' && old.status != 'shipped'".into()),
        partition_key: Some("new.status".into()),
        state: RegistrationState::default(),
    }
}

/// The registration builders bind only text ids and a json document (the
/// `$n::jsonb` param) — map each to the tokio-postgres value the server expects
/// (a `text` id → `String`; the document → a `jsonb` `serde_json::Value`).
fn bind_of(v: &SqlValue) -> Box<dyn ToSql + Sync> {
    match v {
        SqlValue::Text(s) => Box::new(s.clone()),
        SqlValue::Json(s) => {
            Box::new(serde_json::from_str::<serde_json::Value>(s).expect("document is valid json"))
        }
        other => panic!("registration builders bind only text/json params, got {other:?}"),
    }
}

async fn run(client: &Client, c: &Compiled) -> Vec<tokio_postgres::Row> {
    let owned: Vec<Box<dyn ToSql + Sync>> = c.params().iter().map(bind_of).collect();
    let params: Vec<&(dyn ToSql + Sync)> = owned.iter().map(|b| b.as_ref()).collect();
    client
        .query(c.sql(), &params)
        .await
        .unwrap_or_else(|e| panic!("statement failed: {}\n{e}", c.sql()))
}

async fn connect(cfg: &tokio_postgres::Config) -> Client {
    let (client, conn) = cfg.connect(NoTls).await.expect("connect");
    tokio::spawn(async move {
        let _ = conn.await;
    });
    client
}

#[tokio::test]
async fn live_registration_crud_and_rls_isolation() {
    let Some(url) = superuser_url() else {
        eprintln!("WAMN_API_PG_URL unset — skipping the EVT-REG live-apply gate");
        return;
    };
    let base: tokio_postgres::Config = url.parse().expect("valid WAMN_API_PG_URL");

    // ---- preamble (superuser): hermetic reset + the REAL storage SQL --------
    let su = connect(&base).await;
    su.batch_execute(
        "DROP SCHEMA IF EXISTS catalog CASCADE; \
         DO $$ BEGIN IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'wamn_app') \
           THEN CREATE ROLE wamn_app LOGIN PASSWORD 'wamn_app'; END IF; END $$;",
    )
    .await
    .expect("reset catalog schema + ensure wamn_app role");
    su.batch_execute(CATALOG_SCHEMA)
        .await
        .expect("apply deploy/sql/catalog-schema.sql (the storage target)");

    // ---- the real code path: validate, then CRUD as wamn_app + a claim ------
    let cat = catalog();
    let r1 = reg("on-order-shipped", "sales_orders");
    validate(&r1, &cat).expect("registration validates before write");

    let mut app_cfg = base.clone();
    app_cfg.user("wamn_app").password("wamn_app");
    let app = connect(&app_cfg).await;
    app.batch_execute("SET app.tenant = 't1'")
        .await
        .expect("set tenant claim t1");

    // CREATE — RETURNING echoes the denormalized keys + the stored document.
    let rows = run(
        &app,
        &registration::create(
            "shop",
            &r1.registration_id,
            &r1.flow_id,
            &r1.entity,
            &r1.to_json(),
        ),
    )
    .await;
    assert_eq!(rows.len(), 1, "create returns the row");
    assert_eq!(rows[0].get::<_, String>("entity_id"), "sales_orders");
    let stored: serde_json::Value = rows[0].get("registration");
    assert_eq!(
        EventRegistration::from_json(&stored.to_string()).unwrap(),
        r1,
        "the stored document round-trips to the registration"
    );

    // LIST — the one row, tenant-scoped.
    assert_eq!(run(&app, &registration::list("shop")).await.len(), 1);

    // GET — by id.
    let got = run(&app, &registration::get("shop", "on-order-shipped")).await;
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].get::<_, String>("flow_id"), "notify");

    // UPDATE — re-point at a different entity + flow; RETURNING reflects it.
    let r1b = {
        let mut r = r1.clone();
        r.flow_id = "audit".into();
        r.entity = "line_items".into();
        r
    };
    validate(&r1b, &cat).expect("updated registration validates");
    let upd = run(
        &app,
        &registration::update(
            "shop",
            "on-order-shipped",
            &r1b.flow_id,
            &r1b.entity,
            &r1b.to_json(),
        ),
    )
    .await;
    assert_eq!(upd.len(), 1);
    assert_eq!(upd[0].get::<_, String>("entity_id"), "line_items");
    assert_eq!(upd[0].get::<_, String>("flow_id"), "audit");

    // RLS ISOLATION — a different tenant sees none of t1's rows.
    app.batch_execute("SET app.tenant = 't2'")
        .await
        .expect("switch to tenant t2");
    assert_eq!(
        run(&app, &registration::list("shop")).await.len(),
        0,
        "RLS: tenant t2 sees none of t1's registrations"
    );
    assert_eq!(
        run(&app, &registration::get("shop", "on-order-shipped"))
            .await
            .len(),
        0,
        "RLS: tenant t2 cannot read t1's registration by id"
    );

    // DELETE — back as t1; RETURNING the id proves a row was removed.
    app.batch_execute("SET app.tenant = 't1'")
        .await
        .expect("switch back to tenant t1");
    let del = run(&app, &registration::delete("shop", "on-order-shipped")).await;
    assert_eq!(del.len(), 1, "delete removes the row");
    assert_eq!(
        del[0].get::<_, String>("registration_id"),
        "on-order-shipped"
    );
    assert_eq!(run(&app, &registration::list("shop")).await.len(), 0);

    // teardown — leave nothing behind.
    su.batch_execute("DROP SCHEMA IF EXISTS catalog CASCADE")
        .await
        .expect("teardown catalog schema");
}
