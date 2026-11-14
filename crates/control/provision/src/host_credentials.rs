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
    /// Optional host variable for the credential.
    pub host_variable: Option<&'static str>,
    pub database: CredentialDatabase,
}

/// Environment credentials in host variable order.
/// Secret names derive from the family and environment coordinate.
pub const HOST_CREDENTIALS: [HostCredential; 7] = [
    HostCredential {
        family: WorkloadRoleFamily::Query,
        host_variable: None,
        database: CredentialDatabase::Project,
    },
    HostCredential {
        family: WorkloadRoleFamily::App,
        host_variable: Some("WAMN_PG_URL"),
        database: CredentialDatabase::Project,
    },
    HostCredential {
        family: WorkloadRoleFamily::IdentityReader,
        host_variable: Some("WAMN_SYSTEM_URL"),
        database: CredentialDatabase::Control,
    },
    HostCredential {
        family: WorkloadRoleFamily::ConnectionReader,
        host_variable: Some("WAMN_CONNECTION_STORE_URL"),
        database: CredentialDatabase::Control,
    },
    HostCredential {
        family: WorkloadRoleFamily::ExecutorPlatform,
        host_variable: Some("WAMN_EXECUTOR_PLATFORM_PG_URL"),
        database: CredentialDatabase::Project,
    },
    HostCredential {
        family: WorkloadRoleFamily::HttpAdmitter,
        host_variable: Some("WAMN_HTTP_ADMITTER_PG_URL"),
        database: CredentialDatabase::Project,
    },
    HostCredential {
        family: WorkloadRoleFamily::Administration,
        host_variable: Some("WAMN_ADMINISTRATION_PG_URL"),
        database: CredentialDatabase::Project,
    },
];

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::HOST_CREDENTIALS;

    #[test]
    fn credential_names_and_variables_are_unique() {
        let mut names = BTreeSet::new();
        let mut variables = BTreeSet::new();
        for credential in HOST_CREDENTIALS {
            assert!(names.insert(credential.family.label()));
            if let Some(variable) = credential.host_variable {
                assert!(variables.insert(variable));
            }
        }
    }
}
