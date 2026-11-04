//! File fixture assembly without Kubernetes.
#[path = "support/native_file_fixture.rs"]
pub mod native_file_fixture;
#[path = "support/release_documents.rs"]
pub mod release_documents;
mod native_files {
    use super::native_file_fixture::{build_fixture, release_request};
    use super::release_documents::{ORG, PROJECT};
    use std::path::Path;
    /// The file fixture release must have an http role, so that file.expose has a
    /// workload on port 80. This needs no kind cluster.
    #[tokio::test]
    #[ignore = "requires: bin:/usr/lib/postgresql/18/bin/postgres, env:WAMN_BLOB_PUT_COMPONENT, bin:wamn-component-virtualizer"]
    async fn file_fixture_assembles_an_http_role_without_kind() -> anyhow::Result<()> {
        wamn_test_postgres::require_ignored_prerequisites(
            "requires: bin:/usr/lib/postgresql/18/bin/postgres, env:WAMN_BLOB_PUT_COMPONENT, bin:wamn-component-virtualizer",
        );
        let mut postgres = wamn_test_postgres::start(&[])?;
        let database = postgres.create_database("file_http_role")?;
        let repository = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../..")
            .canonicalize()?;
        let work =
            std::env::temp_dir().join(format!("file-http-role-proof-{}", std::process::id()));
        let (package, admissions) = Box::pin(build_fixture(
            &work,
            &work.join("kubeconfig"),
            &repository,
            database.url(),
        ))
        .await?;
        let published = wamn_release_control::publish_release::assemble_local_release(
            &release_request()?,
            &[package],
            &admissions,
        )?
        .published;
        let roles =
            wamn_release_control::release_chart::roles(&published.manifest, ORG, PROJECT, "files");
        assert!(
            roles
                .iter()
                .any(|role| role.name == wamn_release_control::release_chart::RoleName::Http),
            "the file fixture release has an http role"
        );
        let (_, attachment) = published
            .manifest
            .every_attachment()
            .find(|(id, _)| *id == "fixture-run-http")
            .expect("authored fixture route");
        assert_eq!(
            attachment.attachment_type(),
            wamn_catalog::AttachmentType::Http
        );
        std::fs::remove_dir_all(work)?;
        Ok(())
    }
}
