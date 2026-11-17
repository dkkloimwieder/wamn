//! Apply refusals against project integrations and environment connections.

use serde_json::json;

async fn connect(url: &str) -> sqlx::PgConnection {
    let client = wamn_postgres::connect(url).await.unwrap();

    client
}

#[sqlx::test(migrations = false)]
async fn publish_refuses_wasm_and_native_worlds_without_health(pool: sqlx::PgPool) {
    let database = wamn_test_postgres::url(&pool);
    wamn_provision::test_database::system(database.as_str()).await;
    let mut client = connect(database.as_str()).await;
    sqlx::raw_sql("SET ROLE wamn_system; INSERT INTO registry.integration_type(code,runtime_type,exclusive) VALUES ('native-health','native',false); SET ROLE wamn_control").execute(&mut client).await.map(drop).unwrap();
    for code in ["modbus", "native-health"] {
        let result = sqlx::query("SELECT registry.publish_integration($1,2,$2,$3,$4,$5,'{}')").bind(&code).bind(&(format!("sha256:{}", "a".repeat(64))).parse::<wamn_digest::Digest>().unwrap()).bind(&json!({"type":"object","properties":{"authority":{"type":"string","immutable":true}},"required":["authority"]})).bind(&"wamn:modbus/client@1.0.0").bind(&vec!["wamn:modbus/read@1.0.0".to_owned()]).fetch_one(&mut client).await;
        assert!(result.is_err());
    }
}

#[sqlx::test(migrations = false)]
async fn publish_refuses_write_interfaces_outside_the_exports(pool: sqlx::PgPool) {
    let database = wamn_test_postgres::url(&pool);
    wamn_provision::test_database::system(database.as_str()).await;
    let mut client = connect(database.as_str()).await;
    sqlx::raw_sql("SET ROLE wamn_control")
        .execute(&mut client)
        .await
        .map(drop)
        .unwrap();
    let result = sqlx::query(
        "SELECT registry.publish_integration('http',2,NULL,$1,'wamn:connection/http',$2,$3)",
    )
    .bind(&json!({"type":"object","properties":{}}))
    .bind(&vec!["wamn:connection/http@0.1.0"])
    .bind(&vec!["wamn:modbus/write@1.0.0"])
    .fetch_one(&mut client)
    .await;
    assert!(result.is_err());
}
