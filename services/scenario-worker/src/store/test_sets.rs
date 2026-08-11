//! Immutable storage for exact inline test-set bytes.

use anyhow::{Context as _, bail};
use tokio_postgres::GenericClient;
use wamn_authoring_model::{TestSetIdentity, TestSetInput};
use wamn_scenario_model::TestSetDocument;

use crate::authoring::sha256;

/// Insert exact bytes once under their content identity.
///
/// Params: tenant id, test-set hash, schema version, exact bytes, byte length.
pub fn insert_test_set_sql() -> &'static str {
    "INSERT INTO authoring_test_sets \
       (tenant_id, test_set_hash, schema_version, exact_bytes, byte_length) \
     VALUES ($1, $2, $3, $4, $5) \
     ON CONFLICT (tenant_id, test_set_hash) DO NOTHING"
}

/// Read the immutable row used to prove an idempotent conflict is byte-exact.
///
/// Params: tenant id, test-set hash.
pub fn select_test_set_sql() -> &'static str {
    "SELECT schema_version, exact_bytes, byte_length \
       FROM authoring_test_sets \
      WHERE tenant_id = $1 AND test_set_hash = $2"
}

#[derive(Debug)]
struct PreparedTestSet<'a> {
    identity: TestSetIdentity,
    schema_version: String,
    exact_bytes: &'a [u8],
    byte_length: i32,
}

fn prepare_test_set(input: &TestSetInput) -> anyhow::Result<PreparedTestSet<'_>> {
    let document = TestSetDocument::from_definition(&input.definition)
        .context("refuse malformed test-set definition")?;
    let exact_bytes = input.definition.as_bytes();
    let byte_length = i32::try_from(exact_bytes.len())
        .context("test-set byte length exceeds storage integer domain")?;
    Ok(PreparedTestSet {
        identity: TestSetIdentity {
            hash: sha256(exact_bytes),
        },
        schema_version: document.schema_version,
        exact_bytes,
        byte_length,
    })
}

/// Validate and store one exact inline definition, idempotent by content hash.
pub async fn store_test_set<C>(
    client: &C,
    tenant_id: &str,
    input: &TestSetInput,
) -> anyhow::Result<TestSetIdentity>
where
    C: GenericClient + Sync,
{
    if tenant_id.is_empty() {
        bail!("test-set tenant id must not be empty");
    }
    let prepared = prepare_test_set(input)?;
    client
        .execute(
            insert_test_set_sql(),
            &[
                &tenant_id,
                &prepared.identity.hash,
                &prepared.schema_version,
                &prepared.exact_bytes,
                &prepared.byte_length,
            ],
        )
        .await
        .context("store exact authoring test-set bytes")?;

    let row = client
        .query_opt(
            select_test_set_sql(),
            &[&tenant_id, &prepared.identity.hash],
        )
        .await
        .context("read stored authoring test-set bytes")?
        .context("stored authoring test set disappeared")?;
    let stored_schema_version: String = row.get(0);
    let stored_exact_bytes: Vec<u8> = row.get(1);
    let stored_byte_length: i32 = row.get(2);
    if stored_schema_version != prepared.schema_version
        || stored_exact_bytes != prepared.exact_bytes
        || stored_byte_length != prepared.byte_length
    {
        bail!(
            "stored authoring test set does not match exact bytes for {}",
            prepared.identity.hash
        );
    }
    Ok(prepared.identity)
}

#[cfg(test)]
mod tests {
    use wamn_authoring_model::TestSetInput;

    use super::{insert_test_set_sql, prepare_test_set, select_test_set_sql};

    const AUTHORING_DDL: &str = include_str!("../../../../deploy/sql/flow-tests.sql");

    fn input() -> TestSetInput {
        TestSetInput {
            definition: concat!(
                "{\"schema-version\":\"0.1\",\"cases\":[",
                "{\"case-id\":\"one\",\"input\":{},",
                "\"expect\":[{\"run-terminal-outcome\":{\"status\":\"completed\"}}]}",
                "]}"
            )
            .into(),
        }
    }

    #[test]
    fn preparation_keeps_exact_bytes_and_identity() {
        let input = input();
        let prepared = prepare_test_set(&input).expect("valid test set is prepared");
        assert_eq!(prepared.exact_bytes, input.definition.as_bytes());
        assert_eq!(prepared.byte_length as usize, input.definition.len());
        assert_eq!(prepared.schema_version, "0.1");
        assert_eq!(
            prepared.identity.hash,
            "sha256:5a9dc9fa707c2bde275b9d41dede742a166fe22d3db845f00028a054e7e79c0a"
        );

        let mut spaced = input.clone();
        spaced.definition.push('\n');
        assert_ne!(
            prepare_test_set(&spaced)
                .expect("trailing JSON whitespace remains valid")
                .identity,
            prepared.identity
        );
    }

    #[test]
    fn statements_store_once_and_read_back_for_exact_comparison() {
        let insert = insert_test_set_sql();
        assert!(insert.contains("exact_bytes"));
        assert!(insert.contains("ON CONFLICT (tenant_id, test_set_hash) DO NOTHING"));
        assert!(!insert.to_ascii_uppercase().contains("DO UPDATE"));

        let select = select_test_set_sql();
        assert!(select.contains("schema_version, exact_bytes, byte_length"));
        assert!(select.contains("tenant_id = $1 AND test_set_hash = $2"));
    }

    #[test]
    fn ddl_pins_hash_size_utf8_schema_and_immutability_guards() {
        for required in [
            "CREATE TABLE wamn_run.authoring_test_sets",
            "PRIMARY KEY (tenant_id, test_set_hash)",
            "CHECK (schema_version = '0.1')",
            "byte_length BETWEEN 1 AND 1048576",
            "byte_length = octet_length(exact_bytes)",
            "test_set_hash = 'sha256:' || encode(sha256(exact_bytes), 'hex')",
            "authoring_test_sets_update_immutable",
            "authoring_test_sets_delete_immutable",
            "GRANT SELECT, INSERT ON wamn_run.authoring_test_sets TO wamn_scenario_author",
        ] {
            assert!(
                AUTHORING_DDL.contains(required),
                "missing DDL guard {required:?}"
            );
        }
        let normalized_ddl = AUTHORING_DDL
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        assert!(
            normalized_ddl.contains(
                "(convert_from(exact_bytes, 'UTF8')::jsonb ->> 'schema-version') \
                 IS NOT DISTINCT FROM schema_version"
            ),
            "schema-version self-description check must reject SQL NULL"
        );
    }
}
