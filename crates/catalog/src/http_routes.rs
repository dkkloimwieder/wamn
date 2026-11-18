//! Project the release's application routes for publication.
pub use crate::http_route_data::*;
use crate::{
    AttachmentAuthPolicy, AttachmentType, OperationType, ServingManifest, ServingRoute,
    parse_attachment_auth_policy,
};

const ADAPTER_GOVERNED_BYTES: u64 = u32::MAX as u64;
/// The only route configuration key read by the HTTP component.
pub const HTTP_ROUTES_CONFIG: &str = "wamn.http.routes";

/// Prepare the route table from an edge bundle manifest.
pub fn route_data(
    manifest: &ServingManifest,
    release_digest: &wamn_digest::Digest,
) -> Vec<RouteDefinition> {
    manifest
        .attachments
        .iter()
        .filter_map(|(id, attachment)| {
            if !matches!(
                attachment.type_,
                AttachmentType::Http | AttachmentType::Studio
            ) {
                return None;
            }
            let operation = manifest.route(
                &attachment.package_id,
                &attachment.component,
                &attachment.operation,
            )?;
            let signs_files = manifest.signs_file_results(
                &operation.package_id,
                &operation.component,
                &operation.operation,
            );
            route_definition(
                id,
                &attachment.definition,
                &attachment.auth_policy,
                operation,
                signs_files,
                release_digest,
            )
        })
        .collect()
}

/// Project a route and its admitted operation into the ingress configuration.
pub fn route_definition(
    attachment_id: &crate::canonical_name::CanonicalName,
    definition: &serde_json::Value,
    auth_policy: &serde_json::Value,
    operation: &ServingRoute,
    signs_files: bool,
    release_digest: &wamn_digest::Digest,
) -> Option<RouteDefinition> {
    let route = definition.get("route")?;
    let body_limit = match definition.get("raw-body-bytes") {
        Some(raw_body_bytes) => {
            let authored = raw_body_bytes.get("maximum")?.as_u64()?;
            u32::try_from(authored).ok()?.into()
        }
        None => ADAPTER_GOVERNED_BYTES,
    };
    Some(RouteDefinition {
        read_limits: operation.type_.is_read().then(|| {
            let request_element_schema = &definition["input-schema"]["items"];
            let properties = &request_element_schema["properties"];
            let filter = &properties["filter"];
            ReadLimits {
                page_minimum: properties["limit"]["minimum"]
                    .as_u64()
                    .and_then(|n| u32::try_from(n).ok()),
                page_maximum: properties["limit"]["maximum"]
                    .as_u64()
                    .and_then(|n| u32::try_from(n).ok())
                    .map(|n| n.min(50_000)),
                band_starts: filter["properties"]
                    .as_object()
                    .into_iter()
                    .flatten()
                    .filter(|(_, schema)| {
                        schema["required"]
                            .as_array()
                            .is_some_and(|fields| fields.iter().any(|field| field == "min"))
                    })
                    .map(|(field, _)| BandStart {
                        field: field.clone(),
                        required: filter["required"]
                            .as_array()
                            .is_some_and(|fields| fields.iter().any(|required| required == field)),
                    })
                    .collect(),
            }
        }),
        operation_deadline_ms: 30_000,
        sealed_operation_id: operation.operation.to_string(),
        auth_policy: auth_policy.clone(),
        idempotency_rule: (!operation.type_.is_read()).then(|| {
            operation
                .idempotency
                .clone()
                .unwrap_or_else(|| "request_id".to_owned())
        }),
        etag_inputs: (!signs_files).then_some(operation).and_then(|route| {
            let type_ = match route.type_ {
                OperationType::Get => EtagType::Get(route.revision.clone()?),
                OperationType::Query | OperationType::Projection if !route.reads.is_empty() => {
                    EtagType::List(
                        route
                            .reads
                            .iter()
                            .map(|relation| Relation {
                                schema: relation.schema.clone(),
                                name: relation.relation.clone(),
                            })
                            .collect(),
                    )
                }
                _ => return None,
            };
            Some(EtagInputs {
                release: release_digest.to_owned(),
                type_,
            })
        }),
        attachment_id: attachment_id.to_string(),
        path: route.get("path")?.as_str()?.to_string(),
        method: route.get("method")?.as_str()?.to_string(),
        body_limit,
        cache_control: (!signs_files)
            .then_some(operation.type_)
            .and_then(|type_| read_cache_control(type_, parse_attachment_auth_policy(auth_policy))),
    })
}

/// The Cache-Control value of a successful read response
/// (`docs/architecture/execution.md`), or `None` for a type that is not
/// a read. A read is private unless its route admits anonymous callers.
fn read_cache_control(
    type_: OperationType,
    policy: Option<AttachmentAuthPolicy<'_>>,
) -> Option<String> {
    let target = if policy == Some(AttachmentAuthPolicy::None) {
        "public"
    } else {
        "private"
    };
    match type_ {
        OperationType::Get => Some(format!("{target}, no-cache")),
        OperationType::Query | OperationType::Projection => {
            Some(format!("{target}, max-age=10, stale-while-revalidate=60"))
        }
        OperationType::Create
        | OperationType::Update
        | OperationType::Delete
        | OperationType::Command
        | OperationType::EventHandler => None,
    }
}
