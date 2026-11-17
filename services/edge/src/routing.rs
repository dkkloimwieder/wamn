//! The edge's manifest-based route policy, retained until the edge design.
use crate::release::EdgeRelease;
use wamn_catalog::{AttachmentRef, AttachmentType, ServedAttachment};
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
        let read = match attachment {
            ServedAttachment::Authored(AttachmentRef::Route(route)) => manifest
                .route(&route.package_id, &route.component, &route.operation)
                .is_some_and(|route| route.type_.is_read()),
            ServedAttachment::Host(route) => route.route.type_.is_read(),
        };
        Some(RoutePolicy {
            policy: attachment.auth_policy(),
            read,
        })
    }
}
