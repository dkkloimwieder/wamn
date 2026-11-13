//! Final publication documents. Readers do not resolve authored references.
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::Path;
use wamn_catalog::ServingAttachment;

pub use crate::generate::publication::{ATTACHMENTS, COMPONENTS};

/// Read final publication documents from the compiler output.
pub fn read(root: &Path, relative: &str) -> anyhow::Result<Value> {
    let relative = Path::new(relative).strip_prefix("generated")?;
    Ok(serde_json::from_slice(&std::fs::read(
        crate::output_root(root).join(relative),
    )?)?)
}

/// Read the final attachment map.
pub fn read_package_attachments(
    root: &Path,
) -> anyhow::Result<BTreeMap<String, ServingAttachment>> {
    Ok(serde_json::from_value(read(root, ATTACHMENTS)?)?)
}

/// Compile authored publication once, then replace generated documents with final documents.
pub(crate) fn complete(
    root: &Path,
    manifest: &crate::manifest::PackageManifest,
    package: &mut crate::GeneratedPackage,
) -> anyhow::Result<()> {
    use crate::operation_reference::{self, AuthoredDocument};
    let owners = manifest.operation_owners();
    let mut attachments: serde_json::Map<String, Value> = serde_json::from_slice(
        package
            .file(ATTACHMENTS)
            .expect("attachment emitter ran")
            .bytes(),
    )?;
    let mut components: serde_json::Map<String, Value> = serde_json::from_slice(
        package
            .file(COMPONENTS)
            .expect("component emitter ran")
            .bytes(),
    )?;
    let mut authored = Value::Object(operation_reference::read_attachments_document(
        &root.join("publication/attachments.json"),
    )?);
    operation_reference::resolve_authored_document(
        &mut authored,
        AuthoredDocument::Attachments,
        &owners,
    )?;
    for (id, mut attachment) in authored
        .as_object_mut()
        .into_iter()
        .flat_map(std::mem::take)
    {
        anyhow::ensure!(
            !attachments.contains_key(&id),
            "attachment {id} is already generated"
        );
        anyhow::ensure!(
            attachment.is_object() && attachment["definition"].is_object(),
            "attachment {id} must contain a definition object"
        );
        let identity = attachment["registered-operation"].as_str();
        if let Some(description) = package
            .operations()
            .values()
            .find(|description| Some(description.operation.operation.as_str()) == identity)
        {
            attachment["definition"]["input-schema"] =
                crate::generate::publication::input_schema(description);
        }
        let route = attachment
            .pointer("/definition/route/path")
            .and_then(Value::as_str);
        if let Some(path) = route {
            let normalized = wamn_schema_control::exposure::normalize_http_route(
                &wamn_schema_control::exposure::HttpRoute {
                    path: path.to_owned(),
                    method: "POST".to_owned(),
                },
                &id,
            )?;
            attachment["definition"]["route"]["path"] = Value::String(normalized.path);
        }
        attachment["definition-hash"] = Value::String(wamn_digest::canonical_json_sha256(
            &attachment["definition"],
        ));
        attachments.insert(id, attachment);
    }
    let directory = root.join("publication/components");
    if directory.is_dir() {
        let mut paths = std::fs::read_dir(directory)?
            .map(|entry| entry.map(|entry| entry.path()))
            .collect::<Result<Vec<_>, _>>()?;
        paths.sort();
        for path in paths {
            if !path.is_file() {
                continue;
            }
            let mut declaration: Value = serde_json::from_slice(&std::fs::read(&path)?)?;
            operation_reference::resolve_authored_document(
                &mut declaration,
                AuthoredDocument::Declaration,
                &owners,
            )?;
            let component = declaration["component"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("{} has no component", path.display()))?
                .to_owned();
            if let Some(generated) = components.remove(&component) {
                let entries = declaration["operations"]
                    .as_object_mut()
                    .ok_or_else(|| anyhow::anyhow!("component {component} has no operations"))?;
                for (identity, operation) in
                    generated["operations"].as_object().into_iter().flatten()
                {
                    // Compiled operation schemas own the public shape. Authored entries
                    // retain output ports and parameters used by custom operations.
                    if let Some(authored) = entries.get_mut(identity) {
                        authored["input-ports"] = operation["input-ports"].clone();
                        authored["output-ports"] = operation["output-ports"].clone();
                    } else {
                        entries.insert(identity.clone(), operation.clone());
                    }
                }
            }
            components.insert(component, declaration);
        }
    }
    let wirings = root.join("publication/wirings");
    if wirings.is_dir() {
        for entry in std::fs::read_dir(wirings)? {
            let path = entry?.path();
            if !path.is_file() {
                continue;
            }
            let mut document: Value = serde_json::from_slice(&std::fs::read(&path)?)?;
            operation_reference::resolve_authored_document(
                &mut document,
                AuthoredDocument::Wiring,
                &owners,
            )?;
            let name = path
                .file_name()
                .and_then(|name| name.to_str())
                .ok_or_else(|| anyhow::anyhow!("wiring filename is not UTF-8"))?;
            package.replace_json(&format!("generated/publication/wirings/{name}"), &document)?;
        }
    }
    let attachments = Value::Object(attachments);
    let components = Value::Object(components);
    let openapi = crate::generate::publication::openapi(
        manifest,
        package.operations(),
        &attachments,
        &components,
    );
    package.replace_json(ATTACHMENTS, &attachments)?;
    package.replace_json(COMPONENTS, &components)?;
    package.replace_json("generated/openapi.json", &openapi)?;
    Ok(())
}
