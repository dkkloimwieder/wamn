//! The non-secret environment authority in the provisioning namespace.

use std::path::Path;

use anyhow::{Context as _, ensure};
use serde_json::{Value, json};

use super::{ApplicationSelection, EnvironmentDocument};
use crate::environment::{DEFAULT_NAMESPACE, Platform};

// Bound contention without an environment-wide rollout lock.
const WRITE_ATTEMPTS: usize = 4;
const DOCUMENT_KEY: &str = "environment.json";
pub(in crate::environment) const JOB_DOCUMENT: &str = "/environment/environment.json";

fn name(document: &EnvironmentDocument) -> anyhow::Result<String> {
    Ok(
        wamn_control_provision::environment_state::environment_resource_name(
            &document.org,
            &document.project,
            &document.env,
        )?,
    )
}

async fn read(platform: &Platform, name: &str) -> anyhow::Result<Option<Value>> {
    crate::kubectl::get(
        &platform.target.kubeconfig,
        &platform.target.context,
        DEFAULT_NAMESPACE,
        "configmap",
        name,
    )
    .await
}

fn decode(object: &Value, input: &EnvironmentDocument) -> anyhow::Result<EnvironmentDocument> {
    let text = object["data"][DOCUMENT_KEY]
        .as_str()
        .context("environment ConfigMap has no environment.json")?;
    let document = EnvironmentDocument::from_json(text.as_bytes())?;
    ensure!(
        document.triple() == input.triple(),
        "environment ConfigMap coordinate differs"
    );
    Ok(document)
}

impl EnvironmentDocument {
    /// Read the deployed authority and warn about stale configuration input.
    ///
    /// # Errors
    /// Returns an error if the file or the ConfigMap cannot be read.
    pub async fn read_deployed(
        platform: &Platform,
        file: &Path,
        application: Option<&str>,
    ) -> anyhow::Result<Self> {
        let input = Self::compile(file)?;
        Self::read_target(platform, &input, application).await
    }

    /// Read the authority for an already decoded target, including SQL Job inputs.
    ///
    /// # Errors
    /// Returns an error if the ConfigMap is absent or invalid.
    pub async fn read_target(
        platform: &Platform,
        input: &Self,
        application: Option<&str>,
    ) -> anyhow::Result<Self> {
        let name = name(input)?;
        let document = if platform.target.kubeconfig.as_os_str().is_empty() {
            // SQL Jobs read the ConfigMap projection without Kubernetes API credentials.
            let document = Self::compile(Path::new(JOB_DOCUMENT))?;
            ensure!(
                document.triple() == input.triple(),
                "environment ConfigMap coordinate differs"
            );
            document
        } else {
            let object = read(platform, &name).await?.with_context(|| {
                format!("environment ConfigMap {DEFAULT_NAMESPACE}/{name} is absent; provision the environment with env apply first")
            })?;
            decode(&object, input)?
        };
        for warning in stale_warnings(input, &document, application)? {
            eprintln!(
                "WARN stale --env input: {warning}; ConfigMap {DEFAULT_NAMESPACE}/{name} is authoritative"
            );
        }
        Ok(document)
    }

    /// Apply explicit configuration while preserving recorded release pairs.
    ///
    /// # Errors
    /// Returns an error if Kubernetes refuses the write or contention exceeds the bound.
    pub async fn provision_document(&self, platform: &Platform) -> anyhow::Result<Self> {
        save(platform, self, Change::Configuration).await
    }

    /// Record only one application's release pair under its existing application lock.
    pub(in crate::environment) async fn persist_selection(
        &self,
        platform: &Platform,
        application: &str,
    ) -> anyhow::Result<Self> {
        let selection = self
            .applications
            .get(application)
            .context("application disappeared")?;
        save(platform, self, Change::Selection(application, selection)).await
    }
}

enum Change<'a> {
    Configuration,
    Selection(&'a str, &'a ApplicationSelection),
}

async fn save(
    platform: &Platform,
    input: &EnvironmentDocument,
    change: Change<'_>,
) -> anyhow::Result<EnvironmentDocument> {
    let name = name(input)?;
    update(
        input,
        &name,
        read(platform, &name).await?,
        change,
        async || read(platform, &name).await,
        async |object: &Value| {
            let verb = if object["metadata"].get("resourceVersion").is_some() {
                "replace"
            } else {
                "create"
            };
            crate::kubectl::kubectl(
                &platform.target.kubeconfig,
                &platform.target.context,
                DEFAULT_NAMESPACE,
                &[verb, "-f", "-"],
                Some(&serde_json::to_vec(object)?),
            )
            .await
            .map(drop)
        },
    )
    .await
}

async fn update(
    input: &EnvironmentDocument,
    name: &str,
    mut object: Option<Value>,
    change: Change<'_>,
    mut read: impl AsyncFnMut() -> anyhow::Result<Option<Value>>,
    mut write: impl AsyncFnMut(&Value) -> anyhow::Result<()>,
) -> anyhow::Result<EnvironmentDocument> {
    for attempt in 0..WRITE_ATTEMPTS {
        let latest = object
            .as_ref()
            .map(|object| decode(object, input))
            .transpose()?;
        let document = match &change {
            Change::Configuration => {
                let mut configured = input.clone();
                if let Some(latest) = latest {
                    for (name, recorded) in latest.applications {
                        match configured.applications.entry(name) {
                            std::collections::btree_map::Entry::Occupied(mut entry) => {
                                let selection = entry.get_mut();
                                selection.current = recorded.current;
                                selection.previous = recorded.previous;
                            }
                            std::collections::btree_map::Entry::Vacant(entry) => {
                                // Stale input cannot erase another application's selection.
                                entry.insert(recorded);
                            }
                        }
                    }
                }
                configured
            }
            Change::Selection(application, intended) => {
                let mut latest =
                    latest.context("provision the environment before selecting a release")?;
                let selection = latest
                    .applications
                    .get_mut(*application)
                    .context("application disappeared")?;
                selection.current.clone_from(&intended.current);
                selection.previous.clone_from(&intended.previous);
                latest
            }
        };
        let mut object_to_write = object.take().unwrap_or_else(|| {
            json!({
                "apiVersion": "v1", "kind": "ConfigMap",
                "metadata": {"name": name, "namespace": DEFAULT_NAMESPACE},
                "data": {}
            })
        });
        // Preserve the read resourceVersion and metadata, including on conflict retries.
        object_to_write["data"][DOCUMENT_KEY] = serde_json::to_string(&document)?.into();
        match write(&object_to_write).await {
            Ok(()) => return Ok(document),
            Err(error)
                if error.is::<crate::kubectl::Conflict>() && attempt + 1 < WRITE_ATTEMPTS =>
            {
                object = read().await?;
            }
            Err(error) => return Err(error).context("write the environment ConfigMap"),
        }
    }
    unreachable!("the last write attempt returns its result")
}

fn configuration(document: &EnvironmentDocument) -> anyhow::Result<Value> {
    let mut value = serde_json::to_value(document)?;
    for selection in value["applications"]
        .as_object_mut()
        .context("application configuration")?
        .values_mut()
    {
        let selection = selection
            .as_object_mut()
            .context("application configuration")?;
        selection.remove("current");
        selection.remove("previous");
    }
    Ok(value)
}

fn stale_warnings(
    input: &EnvironmentDocument,
    recorded: &EnvironmentDocument,
    application: Option<&str>,
) -> anyhow::Result<Vec<String>> {
    let mut warnings = Vec::new();
    let input_configuration = configuration(input)?;
    let recorded_configuration = configuration(recorded)?;
    if input_configuration != recorded_configuration {
        warnings.push(format!(
            "shared configuration: file={input_configuration}, ConfigMap={recorded_configuration}"
        ));
    }
    for name in application.into_iter().chain(
        recorded
            .applications
            .keys()
            .map(String::as_str)
            .filter(|_| application.is_none()),
    ) {
        let pair = |document: &EnvironmentDocument| {
            document
                .applications
                .get(name)
                .map(|s| json!({"current": s.current, "previous": s.previous}))
        };
        let input = pair(input);
        let recorded = pair(recorded);
        if input != recorded {
            warnings.push(format!(
                "application {name}: file={}, ConfigMap={}",
                serde_json::to_string(&input)?,
                serde_json::to_string(&recorded)?
            ));
        }
    }
    Ok(warnings)
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::*;
    use crate::environment::document::{DeclaredRelease, SchemaMode};

    fn document() -> EnvironmentDocument {
        EnvironmentDocument::from_json(super::super::tests::document().to_string().as_bytes())
            .unwrap()
    }

    fn object(document: &EnvironmentDocument) -> Value {
        json!({"apiVersion":"v1", "kind":"ConfigMap",
            "metadata":{"name":name(document).unwrap(), "namespace":DEFAULT_NAMESPACE,
                "resourceVersion":"1"},
            "data":{DOCUMENT_KEY:serde_json::to_string(document).unwrap()}})
    }

    #[tokio::test]
    async fn stale_resource_version_retries_preserve_both_application_updates() {
        let initial = document();
        let stale = object(&initial);
        let server = RefCell::new(stale.clone());
        let mut intended = initial.clone();
        for (index, selection) in intended.applications.values_mut().enumerate() {
            selection.previous = Some(selection.current.to_string());
            selection.current = DeclaredRelease::Digest(format!("sha256:{index:064x}"));
        }
        let mut write = async |candidate: &Value| {
            let mut server = server.borrow_mut();
            if candidate["metadata"]["resourceVersion"] != server["metadata"]["resourceVersion"] {
                return Err(crate::kubectl::Conflict.into());
            }
            let next = server["metadata"]["resourceVersion"]
                .as_str()
                .unwrap()
                .parse::<u32>()
                .unwrap()
                + 1;
            *server = candidate.clone();
            server["metadata"]["resourceVersion"] = next.to_string().into();
            Ok(())
        };
        for (application, selection) in &intended.applications {
            // Both writers start from the same version. The second must reread and merge.
            update(
                &initial,
                &name(&initial).unwrap(),
                Some(stale.clone()),
                Change::Selection(application, selection),
                async || Ok(Some(server.borrow().clone())),
                &mut write,
            )
            .await
            .unwrap();
        }
        assert_eq!(decode(&server.borrow(), &initial).unwrap(), intended);
    }

    #[tokio::test]
    async fn explicit_configuration_creates_then_preserves_recorded_selections() {
        let initial = document();
        let server = RefCell::new(None);
        let mut write = async |candidate: &Value| {
            *server.borrow_mut() = Some(candidate.clone());
            Ok(())
        };
        update(
            &initial,
            &name(&initial).unwrap(),
            None,
            Change::Configuration,
            async || Ok(server.borrow().clone()),
            &mut write,
        )
        .await
        .unwrap();
        assert_eq!(
            decode(server.borrow().as_ref().unwrap(), &initial).unwrap(),
            initial
        );
        let mut input = initial.clone();
        input.schema.mode = Some(SchemaMode::Migrate);
        for selection in input.applications.values_mut() {
            selection.current = DeclaredRelease::None;
            selection.previous = None;
            selection.hostname = format!("new.{}", selection.hostname);
        }
        let omitted = input.applications.keys().next().unwrap().clone();
        input.applications.remove(&omitted);
        let snapshot = server.borrow().clone();
        let updated = update(
            &input,
            &name(&input).unwrap(),
            snapshot,
            Change::Configuration,
            async || Ok(server.borrow().clone()),
            &mut write,
        )
        .await
        .unwrap();
        assert_eq!(updated.schema, input.schema);
        for (name, recorded) in &initial.applications {
            let updated = &updated.applications[name];
            assert_eq!(updated.current, recorded.current);
            assert_eq!(updated.previous, recorded.previous);
            assert_eq!(
                updated.hostname,
                input.applications.get(name).unwrap_or(recorded).hostname
            );
        }
    }

    #[test]
    fn stale_input_warns_with_both_configuration_and_target_selection_values() {
        let input = document();
        let mut recorded = input.clone();
        recorded.schema.mode = Some(SchemaMode::Migrate);
        for name in input.applications.keys() {
            let selection = recorded.applications.get_mut(name).unwrap();
            selection.current = DeclaredRelease::None;
            selection.previous = None;
            for application in [Some(name.as_str()), None] {
                let warnings = stale_warnings(&input, &recorded, application)
                    .unwrap()
                    .join("\n");
                for value in [
                    configuration(&input).unwrap(),
                    configuration(&recorded).unwrap(),
                ] {
                    assert!(warnings.contains(&value.to_string()));
                }
                assert!(warnings.contains(&input.applications[name].current.to_string()));
                assert!(warnings.contains(&format!("application {name}:")));
                assert!(warnings.contains(
                    &serde_json::to_string(&json!({"current":"none", "previous":null})).unwrap()
                ));
            }
        }
        assert!(stale_warnings(&input, &input, None).unwrap().is_empty());
    }
}
