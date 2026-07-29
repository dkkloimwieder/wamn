//! Pure source and attachment validation for callable-flow exposure.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest as _, Sha256};
use wamn_flow::EntryKind;

/// One release's authored exposure definitions.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ExposureRelease {
    #[serde(default)]
    pub sources: Vec<Source>,
    #[serde(default)]
    pub attachments: Vec<Attachment>,
}

/// A source resolved into an attachment definition.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct Source {
    pub id: String,
    pub kind: SourceKind,
    pub definition: Value,
}

/// Runtime authorization policy resolved by an internal attachment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct CallerPolicy {
    pub allowed_callers: Vec<String>,
}

/// Source classes carried by the Phase 2A spine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SourceKind {
    Auth,
    CallerPolicy,
    Schedule,
}

impl SourceKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auth => "auth",
            Self::CallerPolicy => "caller-policy",
            Self::Schedule => "schedule",
        }
    }
}

/// A release member which exposes one flow.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct Attachment {
    pub id: String,
    pub kind: AttachmentKind,
    pub flow_id: String,
    pub source_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub route: Option<HttpRoute>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mappings: Vec<InputMapping>,
    pub run_deadline_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_deadline_ms: Option<u64>,
}

/// Invocation-source kinds. HTTP, internal, and studio target request entries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AttachmentKind {
    Http,
    Internal,
    Studio,
    Cron,
}

impl AttachmentKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Http => "http",
            Self::Internal => "internal",
            Self::Studio => "studio",
            Self::Cron => "cron",
        }
    }

    fn entry_kind(self) -> EntryKind {
        match self {
            Self::Http | Self::Internal | Self::Studio => EntryKind::Request,
            Self::Cron => EntryKind::Cron,
        }
    }

    fn source_kind(self) -> SourceKind {
        match self {
            Self::Http | Self::Studio => SourceKind::Auth,
            Self::Internal => SourceKind::CallerPolicy,
            Self::Cron => SourceKind::Schedule,
        }
    }
}

/// A normalized HTTP route.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct HttpRoute {
    pub host: String,
    pub path: String,
    pub method: String,
}

/// One transport-to-input mapping.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct InputMapping {
    pub from: MappingSource,
    pub name: String,
    pub to: String,
    #[serde(default)]
    pub optional: bool,
    #[serde(default)]
    pub cardinality: Cardinality,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MappingSource {
    Body,
    Path,
    Query,
    Header,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Cardinality {
    #[default]
    One,
    Many,
}

/// The flow identity data needed to resolve an attachment.
#[derive(Debug, Clone, Copy)]
pub struct FlowExposure<'a> {
    pub flow_id: &'a str,
    pub entry_kind: EntryKind,
    pub artifact_hash: &'a str,
}

/// A validated attachment with its complete resolved definition hash.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedAttachment {
    pub attachment: Attachment,
    pub definition_hash: String,
    pub normalized_host: Option<String>,
    pub normalized_path: Option<String>,
    pub normalized_template: Option<String>,
    pub normalized_method: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExposureError {
    pub code: &'static str,
    pub subject: String,
}

impl fmt::Display for ExposureError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.code, self.subject)
    }
}

impl std::error::Error for ExposureError {}

/// Validate and resolve all exposure definitions into stable database rows.
pub fn resolve_exposure(
    release: &ExposureRelease,
    flows: &[FlowExposure<'_>],
) -> Result<Vec<ResolvedAttachment>, ExposureError> {
    let mut sources = BTreeMap::new();
    for source in &release.sources {
        validate_id(&source.id, "invalid-source-id")?;
        if sources.insert(source.id.as_str(), source).is_some() {
            return Err(error("duplicate-source-id", &source.id));
        }
    }
    let flow_by_id: BTreeMap<_, _> = flows.iter().map(|flow| (flow.flow_id, flow)).collect();
    let mut attachment_ids = BTreeSet::new();
    let mut route_keys = BTreeSet::new();
    let mut resolved = Vec::with_capacity(release.attachments.len());
    for attachment in &release.attachments {
        validate_id(&attachment.id, "invalid-attachment-id")?;
        if !attachment_ids.insert(attachment.id.as_str()) {
            return Err(error("duplicate-attachment-id", &attachment.id));
        }
        let Some(source) = sources.get(attachment.source_id.as_str()) else {
            return Err(error("unknown-source", &attachment.source_id));
        };
        if source.kind != attachment.kind.source_kind() {
            return Err(error("attachment-source-mismatch", &attachment.id));
        }
        if !source.definition.is_object()
            || (source.kind == SourceKind::Schedule
                && !valid_schedule_definition(&source.definition))
            || (source.kind == SourceKind::CallerPolicy && !valid_caller_policy(&source.definition))
        {
            return Err(error("invalid-source-definition", &source.id));
        }
        let Some(flow) = flow_by_id.get(attachment.flow_id.as_str()) else {
            return Err(error("unknown-attachment-flow", &attachment.flow_id));
        };
        if flow.entry_kind != attachment.kind.entry_kind() {
            return Err(error("attachment-entry-kind-mismatch", &attachment.id));
        }
        if attachment.run_deadline_ms == 0
            || attachment
                .response_deadline_ms
                .is_some_and(|response| response == 0 || response > attachment.run_deadline_ms)
        {
            return Err(error("deadline-inversion", &attachment.id));
        }
        let route = validate_route(attachment)?;
        validate_mappings(&attachment.mappings)?;
        if let Some(key) = route.as_ref().map(|route| {
            (
                route.host.clone(),
                canonical_route_template(&route.path),
                route.method.clone(),
            )
        }) {
            if !route_keys.insert(key) {
                return Err(error("ambiguous-http-route", &attachment.id));
            }
        }
        let mut normalized_attachment = attachment.clone();
        normalized_attachment.route = route.clone();
        for mapping in &mut normalized_attachment.mappings {
            if mapping.from == MappingSource::Header {
                mapping.name.make_ascii_lowercase();
            }
        }
        let resolved_json = serde_json::json!({
            "attachment": &normalized_attachment,
            "flow-artifact-hash": flow.artifact_hash,
            "resolved-source": source,
        });
        let definition_hash = hex_sha256(
            &serde_json::to_vec(&resolved_json)
                .map_err(|_| error("exposure-serialization-failed", &attachment.id))?,
        );
        resolved.push(ResolvedAttachment {
            attachment: normalized_attachment,
            definition_hash,
            normalized_host: route.as_ref().map(|route| route.host.clone()),
            normalized_path: route.as_ref().map(|route| route.path.clone()),
            normalized_template: route
                .as_ref()
                .map(|route| canonical_route_template(&route.path)),
            normalized_method: route.as_ref().map(|route| route.method.clone()),
        });
    }
    resolved.sort_by(|a, b| a.attachment.id.cmp(&b.attachment.id));
    Ok(resolved)
}

fn valid_caller_policy(definition: &Value) -> bool {
    serde_json::from_value::<CallerPolicy>(definition.clone()).is_ok_and(|policy| {
        !policy.allowed_callers.is_empty()
            && policy
                .allowed_callers
                .iter()
                .all(|caller| validate_id(caller, "invalid-caller-id").is_ok())
            && policy.allowed_callers.iter().collect::<BTreeSet<_>>().len()
                == policy.allowed_callers.len()
    })
}

fn valid_schedule_definition(definition: &Value) -> bool {
    let Some(object) = definition.as_object() else {
        return false;
    };
    object
        .get("schedule")
        .and_then(Value::as_str)
        .is_some_and(|schedule| !schedule.is_empty())
        && object
            .get("timezone")
            .and_then(Value::as_str)
            .is_some_and(|timezone| !timezone.is_empty())
        && object
            .get("catch-up")
            .and_then(Value::as_str)
            .is_some_and(|catch_up| matches!(catch_up, "skip" | "once"))
}

fn validate_route(attachment: &Attachment) -> Result<Option<HttpRoute>, ExposureError> {
    let needs_route = matches!(
        attachment.kind,
        AttachmentKind::Http | AttachmentKind::Studio
    );
    if needs_route != attachment.route.is_some() {
        return Err(error("attachment-route-mismatch", &attachment.id));
    }
    let Some(route) = &attachment.route else {
        return Ok(None);
    };
    let host = route.host.to_ascii_lowercase();
    if host != "*" && (host.is_empty() || host.contains('/') || host.contains(' ')) {
        return Err(error("invalid-http-host", &attachment.id));
    }
    let method = route.method.to_ascii_uppercase();
    if method.is_empty() || !method.bytes().all(|byte| byte.is_ascii_uppercase()) {
        return Err(error("invalid-http-method", &attachment.id));
    }
    let path = normalize_path(&route.path)
        .ok_or_else(|| error("invalid-http-path-template", &attachment.id))?;
    Ok(Some(HttpRoute { host, path, method }))
}

fn normalize_path(path: &str) -> Option<String> {
    if !path.starts_with('/') || path.contains("//") {
        return None;
    }
    let mut catch_all = false;
    for (index, segment) in path.trim_end_matches('/').split('/').skip(1).enumerate() {
        if catch_all || segment.is_empty() {
            return None;
        }
        if segment.starts_with('{') {
            if !segment.ends_with('}') || segment.len() < 3 {
                return None;
            }
            catch_all = segment.starts_with("{*");
            if catch_all && index + 1 != path.trim_end_matches('/').split('/').skip(1).count() {
                return None;
            }
        } else if segment.contains('{') || segment.contains('}') {
            return None;
        }
    }
    let normalized = path.trim_end_matches('/');
    Some(
        if normalized.is_empty() {
            "/"
        } else {
            normalized
        }
        .to_string(),
    )
}

fn canonical_route_template(path: &str) -> String {
    path.split('/')
        .map(|segment| {
            if segment.starts_with("{*") {
                "{*}"
            } else if segment.starts_with('{') {
                "{}"
            } else {
                segment
            }
        })
        .collect::<Vec<_>>()
        .join("/")
}

fn validate_mappings(mappings: &[InputMapping]) -> Result<(), ExposureError> {
    let protected_headers = [
        "authorization",
        "proxy-authorization",
        "cookie",
        "set-cookie",
    ];
    let mut destinations = BTreeSet::new();
    for mapping in mappings {
        if !valid_pointer(&mapping.to) {
            return Err(error("invalid-input-mapping", &mapping.to));
        }
        if !destinations.insert(mapping.to.as_str()) {
            return Err(error("duplicate-mapping-destination", &mapping.to));
        }
        if mapping.from == MappingSource::Header {
            let name = mapping.name.to_ascii_lowercase();
            if protected_headers.contains(&name.as_str()) || name.starts_with("x-wamn-") {
                return Err(error("protected-header-mapping", &mapping.name));
            }
        }
    }
    for left in &destinations {
        for right in &destinations {
            if left != right
                && !left.is_empty()
                && right.starts_with(*left)
                && right.as_bytes().get(left.len()) == Some(&b'/')
            {
                return Err(error("overlapping-mapping-destination", right));
            }
        }
    }
    Ok(())
}

fn valid_pointer(pointer: &str) -> bool {
    pointer.is_empty()
        || (pointer.starts_with('/')
            && pointer
                .split('/')
                .skip(1)
                .all(|token| !token.contains('~') || valid_pointer_token(token)))
}

fn valid_pointer_token(token: &str) -> bool {
    let mut chars = token.chars();
    while let Some(ch) = chars.next() {
        if ch == '~' && !matches!(chars.next(), Some('0' | '1')) {
            return false;
        }
    }
    true
}

fn validate_id(id: &str, code: &'static str) -> Result<(), ExposureError> {
    if id.is_empty()
        || !id
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        || id.starts_with('-')
        || id.ends_with('-')
    {
        return Err(error(code, id));
    }
    Ok(())
}

fn error(code: &'static str, subject: &str) -> ExposureError {
    ExposureError {
        code,
        subject: subject.to_string(),
    }
}

fn hex_sha256(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request_flow() -> FlowExposure<'static> {
        FlowExposure {
            flow_id: "f1",
            entry_kind: EntryKind::Request,
            artifact_hash: "artifact-f1",
        }
    }

    fn cron_flow() -> FlowExposure<'static> {
        FlowExposure {
            flow_id: "f3",
            entry_kind: EntryKind::Cron,
            artifact_hash: "artifact-f3",
        }
    }

    fn release() -> ExposureRelease {
        ExposureRelease {
            sources: vec![Source {
                id: "erp-keys".into(),
                kind: SourceKind::Auth,
                definition: serde_json::json!({"policy": "api-key"}),
            }],
            attachments: vec![Attachment {
                id: "receive".into(),
                kind: AttachmentKind::Http,
                flow_id: "f1".into(),
                source_id: "erp-keys".into(),
                route: Some(HttpRoute {
                    host: "*".into(),
                    path: "/receipts/".into(),
                    method: "post".into(),
                }),
                mappings: vec![InputMapping {
                    from: MappingSource::Body,
                    name: "body".into(),
                    to: "".into(),
                    optional: false,
                    cardinality: Cardinality::One,
                }],
                run_deadline_ms: 10_000,
                response_deadline_ms: Some(5_000),
            }],
        }
    }

    #[test]
    fn resolves_normalized_route_and_stable_hash() {
        let first = resolve_exposure(&release(), &[request_flow()]).unwrap();
        let second = resolve_exposure(&release(), &[request_flow()]).unwrap();
        assert_eq!(first, second);
        assert_eq!(first[0].normalized_path.as_deref(), Some("/receipts"));
        assert_eq!(first[0].normalized_method.as_deref(), Some("POST"));
    }

    #[test]
    fn resolves_http_internal_studio_and_minimal_cron_definitions() {
        let mut authored = release();
        authored.sources.extend([
            Source {
                id: "callers".into(),
                kind: SourceKind::CallerPolicy,
                definition: serde_json::json!({"allowed-callers": ["f4"]}),
            },
            Source {
                id: "schedule".into(),
                kind: SourceKind::Schedule,
                definition: serde_json::json!({
                    "schedule": "0 2 * * *",
                    "timezone": "America/New_York",
                    "catch-up": "skip"
                }),
            },
        ]);
        let mut internal = authored.attachments[0].clone();
        internal.id = "recommend".into();
        internal.kind = AttachmentKind::Internal;
        internal.source_id = "callers".into();
        internal.route = None;
        let mut studio = authored.attachments[0].clone();
        studio.id = "studio-receive".into();
        studio.kind = AttachmentKind::Studio;
        studio.route.as_mut().unwrap().path = "/studio/receipts".into();
        let mut cron = authored.attachments[0].clone();
        cron.id = "stale-holds".into();
        cron.kind = AttachmentKind::Cron;
        cron.flow_id = "f3".into();
        cron.source_id = "schedule".into();
        cron.route = None;
        cron.mappings.clear();
        cron.response_deadline_ms = None;
        authored.attachments.extend([internal, studio, cron]);
        let resolved = resolve_exposure(&authored, &[request_flow(), cron_flow()]).unwrap();
        assert_eq!(resolved.len(), 4);
        assert!(resolved.iter().any(|item| {
            item.attachment.kind == AttachmentKind::Cron
                && item.normalized_host.is_none()
                && item.normalized_path.is_none()
        }));
    }

    #[test]
    fn caller_policy_is_typed_nonempty_and_unique() {
        let mut authored = release();
        authored.sources[0].kind = SourceKind::CallerPolicy;
        authored.attachments[0].kind = AttachmentKind::Internal;
        authored.attachments[0].route = None;
        authored.attachments[0].mappings.clear();
        authored.sources[0].definition = serde_json::json!({"allowed-callers": []});
        assert_eq!(
            resolve_exposure(&authored, &[request_flow()])
                .unwrap_err()
                .code,
            "invalid-source-definition"
        );

        authored.sources[0].definition = serde_json::json!({"allowed-callers": ["f4", "f4"]});
        assert_eq!(
            resolve_exposure(&authored, &[request_flow()])
                .unwrap_err()
                .code,
            "invalid-source-definition"
        );

        authored.sources[0].definition =
            serde_json::json!({"allowed-callers": ["f4"], "ambient": true});
        assert_eq!(
            resolve_exposure(&authored, &[request_flow()])
                .unwrap_err()
                .code,
            "invalid-source-definition"
        );
    }

    #[test]
    fn rejects_ambiguous_routes_even_when_parameter_names_differ() {
        let mut authored = release();
        let mut other = authored.attachments[0].clone();
        other.id = "receive-other".into();
        other.route.as_mut().unwrap().path = "/{receipt}".into();
        authored.attachments[0].route.as_mut().unwrap().path = "/{id}".into();
        authored.attachments.push(other);
        assert_eq!(
            resolve_exposure(&authored, &[request_flow()])
                .unwrap_err()
                .code,
            "ambiguous-http-route"
        );
    }

    #[test]
    fn rejects_source_entry_mapping_and_deadline_mismatches() {
        let mut authored = release();
        authored.sources[0].kind = SourceKind::Schedule;
        assert_eq!(
            resolve_exposure(&authored, &[request_flow()])
                .unwrap_err()
                .code,
            "attachment-source-mismatch"
        );
        let mut authored = release();
        authored.attachments[0].mappings.push(InputMapping {
            from: MappingSource::Header,
            name: "Authorization".into(),
            to: "/auth".into(),
            optional: false,
            cardinality: Cardinality::One,
        });
        assert_eq!(
            resolve_exposure(&authored, &[request_flow()])
                .unwrap_err()
                .code,
            "protected-header-mapping"
        );
        let mut authored = release();
        authored.attachments[0].response_deadline_ms = Some(20_000);
        assert_eq!(
            resolve_exposure(&authored, &[request_flow()])
                .unwrap_err()
                .code,
            "deadline-inversion"
        );
        let authored = release();
        let wrong_entry = FlowExposure {
            flow_id: "f1",
            entry_kind: EntryKind::Cron,
            artifact_hash: "artifact-f1",
        };
        assert_eq!(
            resolve_exposure(&authored, &[wrong_entry])
                .unwrap_err()
                .code,
            "attachment-entry-kind-mismatch"
        );
    }
}
