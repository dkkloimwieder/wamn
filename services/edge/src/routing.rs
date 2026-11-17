//! The edge's manifest-based route policy, retained until the edge design.
use crate::release::EdgeRelease;
use wamn_catalog::AttachmentType;
use wamn_engine::flow_http_routing::{RoutePolicy, RouteProvider};

impl RouteProvider for EdgeRelease {
    fn target(&self) -> &wamn_registry::ProjectEnvKey {
        &self.release().target().project_env_key
    }
    fn route(&self, id: &wamn_catalog::canonical_name::CanonicalName) -> Option<RoutePolicy<'_>> {
        let manifest = self.release().manifest();
        let attachment = manifest.served_attachment(id)?;
        if !matches!(
            attachment.attachment_type(),
            AttachmentType::Http | AttachmentType::Studio
        ) {
            return None;
        }
        Some(RoutePolicy {
            policy: attachment.auth_policy(),
        })
    }
}
