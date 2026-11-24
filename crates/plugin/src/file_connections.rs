//! Login-targeted lookup of active file connections.

use std::{fmt, sync::Arc};

use uuid::Uuid;
use wamn_application::application_config::ApplicationConfig;
use wamn_blobstore::file_route::{FileConnection, FileError, FileRefusal, failed, refused};

/// Login-targeted registry lookup of active store generations.
/// Existing files use their connection identity without a binding.
pub struct RegistryFileConnections {
    pub release: Arc<ApplicationConfig>,
    pub facts: Arc<wamn_connection::connection_facts::ConnectionFacts>,
}
impl fmt::Debug for RegistryFileConnections {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RegistryFileConnections")
            .field("project", &self.release.project)
            .field("environment", &self.release.environment)
            .finish_non_exhaustive()
    }
}
impl RegistryFileConnections {
    /// Enumerate immutable store generations using the targeted registry reader.
    pub async fn generations(&self) -> Result<Vec<FileConnection>, FileError> {
        let mut generations = Vec::new();
        for (_name, id) in self
            .facts
            .env_connections()
            .await
            .map_err(|error| failed("collector", error))?
            .connections
        {
            let Some(answer) = self
                .facts
                .answer(&id)
                .await
                .map_err(|error| failed("collector", error))?
            else {
                continue;
            };
            if answer.fact.status != wamn_connection::connection_memory::ConnectionStatus::Active {
                continue;
            }
            let generation = self
                .facts
                .generation(&id, answer.fact.active_generation)
                .await
                .map_err(|error| failed("collector", error))?;
            if !matches!(generation.integration_type.as_str(), "gcs" | "s3" | "disk") {
                continue;
            }
            let mut definition = generation.parameters.clone();
            definition["provider"] = serde_json::json!(generation.integration_type);
            generations.push(FileConnection {
                id: id.to_string(),
                generation: answer.fact.active_generation,
                definition,
                read_only: false,
            });
        }
        Ok(generations)
    }

    /// Resolve an application's binding or an existing file's active generation.
    pub(crate) async fn lookup(
        &self,
        application: &wamn_registry::ApplicationName,
        binding: Option<&wamn_package::canonical_name::CanonicalName>,
        pinned: Option<&str>,
    ) -> Result<FileConnection, FileError> {
        if application != &self.release.application {
            return Err(refused(FileRefusal::BindingRefused, "application"));
        }
        let (id, read_only) = match binding {
            None => (
                pinned
                    .and_then(|id| id.parse::<Uuid>().ok())
                    .ok_or_else(|| refused(FileRefusal::BindingRefused, "connection"))?,
                false,
            ),
            Some(name) => {
                let (binding, package) = self
                    .release
                    .components
                    .iter()
                    .find_map(|component| {
                        wamn_application::application_config::binding(
                            &self.release,
                            &component.name,
                            name.as_str(),
                        )
                        .filter(|binding| {
                            binding.interface == "wasmcloud:blobstore"
                                && pinned.is_none_or(|id| id == binding.connection_id)
                        })
                        .map(|binding| (binding, &component.package))
                    })
                    .ok_or_else(|| refused(FileRefusal::BindingRefused, "binding"))?;
                (
                    binding
                        .connection_id
                        .parse()
                        .map_err(|error| failed("binding", error))?,
                    binding
                        .mode
                        .read_only(package, &binding.name)
                        .ok_or_else(|| refused(FileRefusal::BindingRefused, "connection mode"))?,
                )
            }
        };
        let answer = self
            .facts
            .answer(&id)
            .await
            .map_err(|error| failed("binding", error))?
            .filter(|answer| {
                answer.fact.status == wamn_connection::connection_memory::ConnectionStatus::Active
            })
            .ok_or_else(|| refused(FileRefusal::BindingRefused, "connection"))?;
        let generation = self
            .facts
            .generation(&id, answer.fact.active_generation)
            .await
            .map_err(|error| failed("binding", error))?;
        // These are native storage providers. A stored file remains readable even
        // when this application has no current declaration for its connection.
        if !matches!(generation.integration_type.as_str(), "gcs" | "s3") {
            return Err(refused(FileRefusal::BindingRefused, "connection"));
        }
        Ok(FileConnection {
            id: id.to_string(),
            generation: answer.fact.active_generation,
            definition: generation.parameters.clone(),
            read_only,
        })
    }
}
