//! Credential Secrets owned by environment apply and delete.

use crate::WorkloadRoleFamily;

/// The database that a credential reaches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CredentialDatabase {
    Project,
    Control,
}

/// One environment credential and its host binding.
#[derive(Debug, Clone, Copy)]
pub struct HostCredential {
    pub family: WorkloadRoleFamily,
    /// Private output file used by environment apply.
    pub file_name: &'static str,
    /// None for the CDC reader, which runs outside the host group.
    pub host_variable: Option<&'static str>,
    pub database: CredentialDatabase,
}

/// Environment credentials in host variable order, followed by the CDC reader.
/// Secret names derive from the family and environment coordinate.
pub const HOST_CREDENTIALS: [HostCredential; 8] = [
    HostCredential {
        family: WorkloadRoleFamily::App,
        file_name: "app.json",
        host_variable: Some("WAMN_PG_URL"),
        database: CredentialDatabase::Project,
    },
    HostCredential {
        family: WorkloadRoleFamily::IdentityReader,
        file_name: "identity-reader.json",
        host_variable: Some("WAMN_SYSTEM_URL"),
        database: CredentialDatabase::Control,
    },
    HostCredential {
        family: WorkloadRoleFamily::ConnectionReader,
        file_name: "conn-reader.json",
        host_variable: Some("WAMN_CONNECTION_STORE_URL"),
        database: CredentialDatabase::Control,
    },
    HostCredential {
        family: WorkloadRoleFamily::ExecutorPlatform,
        file_name: "executor-platform.json",
        host_variable: Some("WAMN_EXECUTOR_PLATFORM_PG_URL"),
        database: CredentialDatabase::Project,
    },
    HostCredential {
        family: WorkloadRoleFamily::HttpAdmitter,
        file_name: "http-admitter.json",
        host_variable: Some("WAMN_HTTP_ADMITTER_PG_URL"),
        database: CredentialDatabase::Project,
    },
    HostCredential {
        family: WorkloadRoleFamily::EventMaterializer,
        file_name: "event-materializer.json",
        host_variable: Some("WAMN_EVENT_MATERIALIZER_PG_URL"),
        database: CredentialDatabase::Project,
    },
    HostCredential {
        family: WorkloadRoleFamily::Administration,
        file_name: "administration.json",
        host_variable: Some("WAMN_ADMINISTRATION_PG_URL"),
        database: CredentialDatabase::Project,
    },
    HostCredential {
        family: WorkloadRoleFamily::RegistryReader,
        file_name: "registry-reader.json",
        host_variable: None,
        database: CredentialDatabase::Control,
    },
];

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::HOST_CREDENTIALS;

    #[test]
    fn credential_names_files_and_variables_are_unique() {
        let mut names = BTreeSet::new();
        let mut files = BTreeSet::new();
        let mut variables = BTreeSet::new();
        for credential in HOST_CREDENTIALS {
            assert!(names.insert(credential.family.label()));
            assert!(files.insert(credential.file_name));
            if let Some(variable) = credential.host_variable {
                assert!(variables.insert(variable));
            }
        }
    }
}
