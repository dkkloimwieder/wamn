//! Read and check the credential files selected by an application test.

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context as _, ensure};
use serde::Deserialize;
use wamn_control_provision::workload_role::WorkloadRoleFamily;

/// The credential files and database endpoint declared by the caller.
#[derive(Debug, Clone)]
pub struct HostSecretsInput {
    pub role_families: Vec<WorkloadRoleFamily>,
    pub guest_secret_file: PathBuf,
    pub namespace: String,
    pub database_host: String,
}

/// A checked credential file reference, without its credential contents.
#[derive(Debug, Clone)]
pub struct HostSecret {
    pub family: WorkloadRoleFamily,
    pub path: PathBuf,
    pub name: String,
}

/// Check the exact selected file set and each Secret's namespace and endpoint.
pub fn derive_host_secrets(
    directory: &Path,
    input: &HostSecretsInput,
) -> anyhow::Result<Vec<HostSecret>> {
    ensure!(
        !input.role_families.is_empty(),
        "host Secrets input declares no role families"
    );
    let mut guest_parts = input.guest_secret_file.components();
    ensure!(
        matches!(guest_parts.next(), Some(Component::Normal(_))) && guest_parts.next().is_none(),
        "guest Secret file must be one file name"
    );
    let mut selected = vec![(WorkloadRoleFamily::App, input.guest_secret_file.clone())];
    for family in &input.role_families {
        ensure!(
            *family != WorkloadRoleFamily::App,
            "the guest Secret is declared separately"
        );
        selected.push((
            *family,
            PathBuf::from(format!("{}.json", family.cli_stem())),
        ));
    }
    let expected = selected
        .iter()
        .map(|(_, path)| path.as_os_str().to_owned())
        .collect::<BTreeSet<_>>();
    ensure!(
        expected.len() == selected.len(),
        "host Secrets input repeats a credential file"
    );
    let mut emitted = BTreeSet::<OsString>::new();
    for entry in std::fs::read_dir(directory)
        .with_context(|| format!("read Secret directory {}", directory.display()))?
    {
        let entry = entry.context("read Secret directory entry")?;
        if entry
            .file_type()
            .context("read Secret file type")?
            .is_file()
            && entry
                .path()
                .extension()
                .is_some_and(|extension| extension == "json")
        {
            emitted.insert(entry.file_name());
        }
    }
    ensure!(
        emitted == expected,
        "host credential files differ from the declaration: emitted {emitted:?}, declared {expected:?}"
    );
    selected
        .into_iter()
        .map(|(family, file)| {
            let path = directory.join(file);
            let bytes =
                std::fs::read(&path).with_context(|| format!("read Secret {}", path.display()))?;
            let secret: CredentialSecret = serde_json::from_slice(&bytes)
                .with_context(|| format!("parse Secret {}", path.display()))?;
            ensure!(
                secret.metadata.namespace == input.namespace,
                "Secret {} is not for namespace {}",
                path.display(),
                input.namespace
            );
            ensure!(
                !secret.metadata.name.is_empty(),
                "Secret {} has an empty name",
                path.display()
            );
            let url = url::Url::parse(&secret.string_data.url).with_context(|| {
                format!("Secret {} has an invalid database URL", path.display())
            })?;
            ensure!(
                url.host_str() == Some(input.database_host.as_str()) && url.port() == Some(5432),
                "Secret {} is not for database host {} at port 5432",
                path.display(),
                input.database_host
            );
            Ok(HostSecret {
                family,
                path,
                name: secret.metadata.name,
            })
        })
        .collect()
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CredentialSecret {
    metadata: SecretMetadata,
    string_data: SecretData,
}

#[derive(Deserialize)]
struct SecretMetadata {
    name: String,
    namespace: String,
}

#[derive(Deserialize)]
struct SecretData {
    url: String,
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};

    use serde_json::json;
    use wamn_control_provision::workload_secret_name;

    use super::{HostSecretsInput, WorkloadRoleFamily, derive_host_secrets};
    use crate::scratch::ScratchRoot;

    fn directory() -> ScratchRoot {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = ScratchRoot(std::env::temp_dir().join(format!(
            "host-secret-tests-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        )));
        std::fs::create_dir(root.path()).unwrap();
        root
    }

    fn input() -> HostSecretsInput {
        HostSecretsInput {
            role_families: vec![
                WorkloadRoleFamily::ExecutorPlatform,
                WorkloadRoleFamily::IdentityReader,
                WorkloadRoleFamily::HttpAdmitter,
                WorkloadRoleFamily::EventMaterializer,
            ],
            guest_secret_file: "guest-sql.json".into(),
            namespace: "warehouse-eu-3".into(),
            database_host: "10.9.8.7".into(),
        }
    }

    fn write_secret(path: &std::path::Path, name: &str, namespace: &str, url: &str) {
        std::fs::write(
            path,
            serde_json::to_vec(&json!({"kind":"Secret","type":"Opaque",
            "metadata":{"name":name,"namespace":namespace},"stringData":{"url":url}}))
            .unwrap(),
        )
        .unwrap();
    }

    fn seed(root: &ScratchRoot, input: &HostSecretsInput) {
        for family in
            std::iter::once(WorkloadRoleFamily::App).chain(input.role_families.iter().copied())
        {
            let file = if family == WorkloadRoleFamily::App {
                input.guest_secret_file.clone()
            } else {
                format!("{}.json", family.cli_stem()).into()
            };
            write_secret(
                &root.path().join(file),
                &workload_secret_name(family, "acme", "receiving", "dev"),
                &input.namespace,
                &format!(
                    "postgresql://user:test-password@{}:5432/database",
                    input.database_host
                ),
            );
        }
    }

    #[test]
    fn selected_families_keep_their_names_paths_and_guest_file() {
        for count in [4, 2] {
            let mut input = input();
            input.role_families.truncate(count);
            let root = directory();
            seed(&root, &input);
            let selected = derive_host_secrets(root.path(), &input).unwrap();
            assert_eq!(selected.len(), count + 1);
            assert_eq!(selected[0].family, WorkloadRoleFamily::App);
            assert_eq!(selected[0].path, root.path().join("guest-sql.json"));
            for secret in selected {
                assert_eq!(
                    secret.name,
                    workload_secret_name(secret.family, "acme", "receiving", "dev")
                );
                assert!(secret.path.is_file());
            }
        }
    }

    #[test]
    fn missing_extra_repeated_or_empty_family_declarations_are_refused() {
        let root = directory();
        let input = input();
        seed(&root, &input);
        std::fs::copy(
            root.path().join("guest-sql.json"),
            root.path().join("retention.json"),
        )
        .unwrap();
        assert!(
            derive_host_secrets(root.path(), &input)
                .unwrap_err()
                .to_string()
                .contains("files differ")
        );
        std::fs::remove_file(root.path().join("retention.json")).unwrap();
        let mut absent = input.clone();
        absent.role_families.push(WorkloadRoleFamily::Retention);
        assert!(
            derive_host_secrets(root.path(), &absent)
                .unwrap_err()
                .to_string()
                .contains("files differ")
        );
        let mut repeated = input.clone();
        repeated.role_families.push(repeated.role_families[0]);
        assert!(
            derive_host_secrets(root.path(), &repeated)
                .unwrap_err()
                .to_string()
                .contains("repeats")
        );
        let mut empty = input;
        empty.role_families.clear();
        assert!(
            derive_host_secrets(root.path(), &empty)
                .unwrap_err()
                .to_string()
                .contains("declares no role families")
        );
    }

    #[test]
    fn every_secret_requires_its_namespace_name_and_database_endpoint() {
        let root = directory();
        let input = input();
        seed(&root, &input);
        let path = root.path().join("http-admitter.json");
        for (name, namespace, url, reason) in [
            (
                "secret",
                "other",
                "postgresql://u:test-password@10.9.8.7:5432/d",
                "namespace",
            ),
            (
                "",
                "warehouse-eu-3",
                "postgresql://u:test-password@10.9.8.7:5432/d",
                "empty name",
            ),
            (
                "secret",
                "warehouse-eu-3",
                "postgresql://u:test-password@10.0.0.1:5432/d",
                "database host",
            ),
            (
                "secret",
                "warehouse-eu-3",
                "postgresql://u:test-password@10.0.0.1:5432/d?next=@10.9.8.7:5432/d",
                "database host",
            ),
            (
                "secret",
                "warehouse-eu-3",
                "postgresql://u:test-password@10.9.8.7:5433/d",
                "port 5432",
            ),
            (
                "secret",
                "warehouse-eu-3",
                "not a URL test-password",
                "invalid database URL",
            ),
        ] {
            write_secret(&path, name, namespace, url);
            let error = derive_host_secrets(root.path(), &input).unwrap_err();
            assert!(format!("{error:#}").contains(reason));
            assert!(!format!("{error:#}").contains("test-password"));
        }
    }

    #[test]
    fn malformed_secret_or_missing_directory_is_refused_without_reading_other_paths() {
        let root = directory();
        let input = input();
        seed(&root, &input);
        std::fs::write(root.path().join("http-admitter.json"), b"{}").unwrap();
        assert!(derive_host_secrets(root.path(), &input).is_err());
        assert!(derive_host_secrets(&root.path().join("absent"), &input).is_err());
        let mut outside = input;
        outside.guest_secret_file = "../guest-sql.json".into();
        assert!(
            derive_host_secrets(root.path(), &outside)
                .unwrap_err()
                .to_string()
                .contains("one file name")
        );
    }

    #[test]
    fn role_secret_names_keep_the_selected_overlay_binding() {
        use crate::rendering::{
            EventIdentity, HostRoleSecret, HostValuesInput, render_host_values,
        };
        let root = directory();
        let input = input();
        seed(&root, &input);
        let mut selected = derive_host_secrets(root.path(), &input)
            .unwrap()
            .into_iter();
        let guest = selected.next().unwrap();
        let host = HostValuesInput {
            namespace: input.namespace,
            host_tag: "test".into(),
            replicas: 3,
            component_artifact_base: "registry.test.invalid/components".into(),
            release_artifact_base: "registry.test.invalid/releases".into(),
            manifest_digest: "sha256:test".into(),
            nats_url: "nats://nats.test.invalid:4222".into(),
            event: EventIdentity {
                org: "acme".into(),
                project: "receiving".into(),
                environment: "dev".into(),
            },
            guest_secret_name: guest.name,
            role_secrets: selected
                .map(|secret| HostRoleSecret {
                    family: secret.family,
                    name: secret.name,
                })
                .collect(),
            object_store_secret_name: None,
        };
        let base = include_str!("../../deploy/platform/values-host-default.yaml");
        let overlay = include_str!("../../deploy/platform/values-host-receiving-pat.yaml");
        assert!(render_host_values(base, overlay, &host).is_ok());
        let mut wrong = host;
        wrong.event.project = "wms".into();
        assert!(
            render_host_values(base, overlay, &wrong)
                .unwrap_err()
                .to_string()
                .contains("missing role Secret")
        );
    }
}
