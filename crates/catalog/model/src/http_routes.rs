//! Project the release's application routes for publication.
pub use crate::http_route_data::*;
use crate::{
    AttachmentAuthPolicy, AttachmentRef, OperationType, ServedAttachment, ServingManifest,
    parse_attachment_auth_policy,
};
const ADAPTER_GOVERNED_BYTES: u64 = u32::MAX as u64;
/// The only route configuration key read by the HTTP component.
pub const HTTP_ROUTES_CONFIG: &str = "wamn.http.routes";
/// Prepare the route table from the release selected for this workload.
pub fn route_data(manifest: &ServingManifest, release_digest: &str) -> Vec<RouteDefinition> {
    manifest
        .every_served_attachment()
        .filter_map(|(id, attachment)| {
            if !matches!(
                attachment.attachment_type(),
                crate::AttachmentType::Http | crate::AttachmentType::Studio
            ) {
                return None;
            }
            if matches!(
                attachment,
                ServedAttachment::Authored(AttachmentRef::Route(_))
            ) {
                route_definition(manifest, id, attachment, release_digest)
            } else {
                None
            }
        })
        .collect()
}
fn route_definition(
    manifest: &ServingManifest,
    attachment_id: &str,
    attachment: ServedAttachment<'_>,
    release_digest: &str,
) -> Option<RouteDefinition> {
    let route = attachment.definition().get("route")?;
    let body_limit = match attachment.definition().get("raw-body-bytes") {
        Some(raw_body_bytes) => {
            let authored = raw_body_bytes.get("maximum")?.as_u64()?;
            u32::try_from(authored).ok()?.into()
        }
        None => ADAPTER_GOVERNED_BYTES,
    };
    let operation = match attachment {
        ServedAttachment::Authored(AttachmentRef::Route(attachment)) => manifest.route(
            &attachment.package_id,
            &attachment.component,
            &attachment.operation,
        ),
        ServedAttachment::Host(_) => None,
    };
    let signs_files = operation.is_some_and(|route| {
        manifest.signs_file_results(&route.package_id, &route.component, &route.operation)
    });
    Some(RouteDefinition {
        read_limits: operation.filter(|route| route.type_.is_read()).map(|_| {
            let request_element_schema = &attachment.definition()["input-schema"]["items"];
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
        sealed_operation_id: operation?.operation.clone(),
        auth_policy: attachment.auth_policy().clone(),
        idempotency_rule: operation
            .filter(|route| !route.type_.is_read())
            .map(|route| {
                route
                    .idempotency
                    .clone()
                    .unwrap_or_else(|| "request_id".to_owned())
            }),
        etag_inputs: operation.filter(|_| !signs_files).and_then(|route| {
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
        cache_control: operation
            .filter(|_| !signs_files)
            .map(|route| route.type_)
            .and_then(|type_| {
                read_cache_control(
                    type_,
                    parse_attachment_auth_policy(attachment.auth_policy()),
                )
            }),
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
