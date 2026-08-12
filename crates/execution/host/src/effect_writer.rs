//! Fixed-path private effect-writer construction.

use std::io::ErrorKind;
use std::time::SystemTime;

use anyhow::Context as _;
use wamn_run_state::{EFFECT_WRITER_CREDENTIAL_PATH, EffectWriterClient, EffectWriterScope};

use crate::ExecutionIdentity;

pub(super) async fn load_effect_writer(
    identity: &ExecutionIdentity<'_>,
) -> anyhow::Result<Option<EffectWriterClient>> {
    let (org, environment, database) = match (identity.org, identity.environment, identity.database)
    {
        (None, None, None) => return Ok(None),
        (Some(org), Some(environment), Some(database)) => (org, environment, database),
        _ => anyhow::bail!(
            "private effect-writer host identity requires org, environment, and database together"
        ),
    };
    let document = match std::fs::read(EFFECT_WRITER_CREDENTIAL_PATH) {
        Ok(document) => document,
        Err(error) if error.kind() == ErrorKind::NotFound => {
            anyhow::bail!("private effect-writer credential is missing from its fixed mount")
        }
        Err(error) => {
            return Err(error).with_context(|| {
                format!("read fixed effect-writer credential {EFFECT_WRITER_CREDENTIAL_PATH}")
            });
        }
    };

    let schema = identity
        .schema
        .context("fixed effect-writer credential requires host-held schema identity")?;
    let client = EffectWriterClient::from_secret_document(
        &document,
        EffectWriterScope {
            tenant_id: identity.tenant,
            org,
            project: identity.project,
            environment,
            database,
            schema,
        },
        SystemTime::now(),
    )
    .await
    .context("construct fixed-scope private effect-writer client")?;
    Ok(Some(client))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credential_path_is_frozen_and_has_no_override_surface() {
        assert_eq!(
            EFFECT_WRITER_CREDENTIAL_PATH,
            "/etc/wamn/effect-writer/credential.json"
        );
        let source = include_str!("effect_writer.rs")
            .split_once("#[cfg(test)]")
            .expect("test module marker")
            .0;
        for forbidden in ["std::env::var", "clap", "DATABASE_URL", "WAMN_PG_URL"] {
            assert!(!source.contains(forbidden));
        }
    }

    #[test]
    fn retained_private_client_has_no_effect_operation_caller() {
        let construction = include_str!("effect_writer.rs")
            .split_once("#[cfg(test)]")
            .expect("test module marker")
            .0;
        let host = include_str!("lib.rs");
        for operation in [".begin_attempt(", ".acquire_dispatch(", ".record_outcome("] {
            assert!(!construction.contains(operation));
            assert!(!host.contains(operation));
        }
    }

    #[test]
    fn schema_is_host_only_and_not_a_credential_override() {
        let source = include_str!("effect_writer.rs")
            .split_once("#[cfg(test)]")
            .expect("test module marker")
            .0;
        assert!(source.contains("let schema = identity"));
        assert!(source.contains("EffectWriterScope"));
        assert!(!source.contains("EffectWriterCredentialScope"));
    }
}
