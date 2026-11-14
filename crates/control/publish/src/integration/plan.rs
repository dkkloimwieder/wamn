use super::{BuiltIntegration, Publication, ensure};

pub(super) fn require_code(code: &str) -> anyhow::Result<()> {
    ensure!(
        code.bytes()
            .next()
            .is_some_and(|byte| byte.is_ascii_lowercase())
            && code.bytes().all(|byte| byte.is_ascii_lowercase()
                || byte.is_ascii_digit()
                || matches!(byte, b'_' | b'-')),
        "integration code must start with a lowercase letter and contain lowercase letters, digits, underscores or hyphens"
    );
    Ok(())
}

pub(super) fn require_write_interfaces(
    publication: &Publication,
    exports: &[String],
) -> anyhow::Result<()> {
    ensure!(
        publication
            .write_interfaces
            .iter()
            .all(|interface| exports.contains(interface)),
        "write_interface_not_exported"
    );
    Ok(())
}

pub(super) fn require_published_manifest(
    built: &BuiltIntegration,
    manifest: &[u8],
    digest: &str,
) -> anyhow::Result<()> {
    ensure!(
        manifest == built.manifest && digest == built.digest,
        "published integration manifest differs from its build"
    );
    Ok(())
}

pub(super) fn require_published_component(
    built: &BuiltIntegration,
    pulled: &[u8],
) -> anyhow::Result<()> {
    ensure!(
        pulled == built.component,
        "published integration bytes differ from its build"
    );
    Ok(())
}
