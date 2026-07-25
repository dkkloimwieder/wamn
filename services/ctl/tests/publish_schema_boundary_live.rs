//! Live PostgreSQL proof for the publish-catalog schema boundary (wamn-5wd1.29).
//!
//! Set `WAMN_CTL_PG_URL` to a superuser URL for a disposable PostgreSQL
//! database. The gate precreates the physical 63-byte schema PostgreSQL would
//! truncate two distinct 64-byte inputs onto, then drives the real
//! `publish_catalog::run` entry point. Both inputs must be rejected before the
//! admin effect path: the sentinel schema keeps exactly one table, no catalog
//! snapshot/run-plane/entity-map artifact appears, and its replica identity is
//! unchanged. A valid publish then proves the normal provision + run-plane path.

use tokio_postgres::{Client, NoTls};

use wamn_ctl::publish_catalog::{self, PublishCatalogArgs};

const VALID_SCHEMA: &str = "publish_boundary_ok";

fn catalog_json() -> &'static str {
    r#"{"schema-version":"0.1","catalog-id":"schema-boundary","version":1,"entities":[
         {"id":"sentinel","name":"sentinel","fields":[
           {"id":"value","name":"value","type":{"kind":"text"}}
         ]}
       ]}"#
}

fn publish_args(catalog: &std::path::Path, url: &str, schema: String) -> PublishCatalogArgs {
    PublishCatalogArgs {
        catalog: catalog.to_path_buf(),
        admin_database_url: Some(url.to_string()),
        tenant: "boundary-tenant".to_string(),
        schema,
        provision: true,
        runstate: true,
        seed_dataset: None,
        flow: vec![],
        skip_reconcile_replica_identity: false,
    }
}

async fn connect(url: &str) -> Client {
    let (client, connection) = tokio_postgres::connect(url, NoTls)
        .await
        .expect("connect to disposable PostgreSQL");
    tokio::spawn(async move {
        let _ = connection.await;
    });
    client
}

async fn relation_names(client: &Client, schema: &str) -> Vec<String> {
    client
        .query(
            "SELECT c.relname FROM pg_class c \
             JOIN pg_namespace n ON n.oid = c.relnamespace \
             WHERE n.nspname = $1 AND c.relkind = 'r' ORDER BY c.relname",
            &[&schema],
        )
        .await
        .expect("read schema relations")
        .iter()
        .map(|row| row.get(0))
        .collect()
}

async fn relreplident(client: &Client, schema: &str, table: &str) -> String {
    client
        .query_one(
            "SELECT c.relreplident::text FROM pg_class c \
             JOIN pg_namespace n ON n.oid = c.relnamespace \
             WHERE n.nspname = $1 AND c.relname = $2",
            &[&schema, &table],
        )
        .await
        .expect("read replica identity")
        .get(0)
}

#[tokio::test]
async fn invalid_overlong_publish_is_pre_effect_and_non_aliasing_live() {
    let Some(url) = std::env::var("WAMN_CTL_PG_URL").ok() else {
        eprintln!("WAMN_CTL_PG_URL unset — skipping the schema-boundary live gate");
        return;
    };

    let client = connect(&url).await;
    let physical = format!("s{}", "a".repeat(62));
    let first = format!("{physical}x");
    let second = format!("{physical}y");
    assert_eq!(physical.len(), 63);
    assert_eq!(first.len(), 64);
    assert_eq!(second.len(), 64);
    assert_ne!(first, second);
    assert_eq!(&first.as_bytes()[..63], &second.as_bytes()[..63]);

    client
        .batch_execute(&format!(
            "DROP SCHEMA IF EXISTS \"{physical}\" CASCADE; \
             DROP SCHEMA IF EXISTS {VALID_SCHEMA} CASCADE; \
             CREATE SCHEMA \"{physical}\"; \
             CREATE TABLE \"{physical}\".sentinel (id bigint PRIMARY KEY); \
             ALTER TABLE \"{physical}\".sentinel REPLICA IDENTITY FULL;"
        ))
        .await
        .expect("prepare the truncation-alias sentinel");

    let catalog = std::env::temp_dir().join(format!(
        "wamn-5wd1-29-publish-schema-{}.json",
        std::process::id()
    ));
    std::fs::write(&catalog, catalog_json()).expect("write catalog fixture");

    for invalid in [first, second] {
        let error = publish_catalog::run(publish_args(&catalog, &url, invalid.clone()))
            .await
            .expect_err("the overlong schema must fail before admin effects");
        let message = format!("{error:#}");
        assert!(
            message.contains("identifier exceeds PostgreSQL's 63-byte limit"),
            "{invalid:?}: {message}"
        );
        assert!(!message.contains("admin connect"), "{invalid:?}: {message}");
    }

    let schemas: i64 = client
        .query_one(
            "SELECT count(*) FROM pg_namespace WHERE nspname = $1",
            &[&physical],
        )
        .await
        .expect("count physical schemas")
        .get(0);
    assert_eq!(schemas, 1, "invalid aliases create no schema residue");
    assert_eq!(
        relation_names(&client, &physical).await,
        ["sentinel"],
        "no table, catalog snapshot, run-plane, or entity-map residue"
    );
    assert_eq!(
        relreplident(&client, &physical, "sentinel").await,
        "f",
        "invalid publish does not run the replica-identity reset"
    );

    publish_catalog::run(publish_args(&catalog, &url, VALID_SCHEMA.to_string()))
        .await
        .expect("valid publish + run-plane provision remains unchanged");
    let relations = relation_names(&client, VALID_SCHEMA).await;
    for expected in [
        "sentinel",
        "wamn_catalog",
        "wamn_entities",
        "runs",
        "flows",
        "test_suites",
    ] {
        assert!(
            relations.iter().any(|relation| relation == expected),
            "valid publish creates {expected}: {relations:?}"
        );
    }
    let snapshot_rows: i64 = client
        .query_one(
            &format!(
                "SELECT count(*) FROM {VALID_SCHEMA}.wamn_catalog \
                 WHERE tenant_id = 'boundary-tenant'"
            ),
            &[],
        )
        .await
        .expect("read valid catalog snapshot")
        .get(0);
    assert_eq!(
        snapshot_rows, 1,
        "valid publish writes the catalog snapshot"
    );
    let entity_map_rows: i64 = client
        .query_one(
            &format!("SELECT count(*) FROM {VALID_SCHEMA}.wamn_entities"),
            &[],
        )
        .await
        .expect("read valid entity map")
        .get(0);
    assert_eq!(entity_map_rows, 1, "valid publish writes the entity map");
    assert_eq!(
        relreplident(&client, VALID_SCHEMA, "sentinel").await,
        "d",
        "valid publish preserves the no-registration DEFAULT identity"
    );

    client
        .batch_execute(&format!(
            "DROP SCHEMA IF EXISTS \"{physical}\" CASCADE; \
             DROP SCHEMA IF EXISTS {VALID_SCHEMA} CASCADE;"
        ))
        .await
        .expect("zero-residue teardown");
    let remaining: i64 = client
        .query_one(
            "SELECT count(*) FROM pg_namespace WHERE nspname = $1 OR nspname = $2",
            &[&physical, &VALID_SCHEMA],
        )
        .await
        .expect("verify teardown")
        .get(0);
    assert_eq!(remaining, 0, "live proof leaves no schema residue");
    std::fs::remove_file(catalog).expect("remove catalog fixture");
}
