//! One real serving release for the gates that drive the materializer guest.
//!
//! The guest's durable-consumer bind is gated on the serving release's
//! registration projection (wamn-0h0g.15.95): a `WamnJetstream` carrying no
//! release refuses EVERY bind, so a gate that drives the real guest observes
//! zero deliveries without one. These fixtures mount one canonical manifest and
//! load it through the production weld — the gate is never stubbed or relaxed.
//!
//! One registration is enough even for a gate that seeds several on the same
//! entity: the guest's filter subject spans every op of its entity
//! (`components/execution/materializer/src/main.rs`), and a wildcard op is
//! admitted on the entity alone (`release_sources` in the jetstream plugin), so
//! any registration sourcing that entity admits every bind over it.

use std::collections::BTreeSet;
use std::sync::Arc;

use anyhow::Context as _;

use wamn_runtime::release_manifest::ReleaseManifestWeld;

/// The release identity and the one registration a gate needs admitted.
#[derive(Debug)]
pub(crate) struct ReleaseFixture<'a> {
    pub(crate) tenant: &'a str,
    pub(crate) catalog: &'a str,
    pub(crate) environment: &'a str,
    pub(crate) registration: &'a str,
    pub(crate) wiring: &'a str,
    pub(crate) entity: &'a str,
    pub(crate) ops: &'a [&'a str],
}

/// Mount `fixture` as canonical bytes and load it through the production weld.
pub(crate) fn load_release(
    fixture: ReleaseFixture<'_>,
) -> anyhow::Result<Arc<ReleaseManifestWeld>> {
    // A `BTreeSet` because the manifest's own `ops` is one and the mounted bytes
    // must ALREADY be canonical: the weld refuses a document it would
    // re-serialize with the ops in another order.
    let ops: BTreeSet<&str> = fixture.ops.iter().copied().collect();
    let document = serde_json::json!({
        "format-version": wamn_catalog::SERVING_MANIFEST_FORMAT_VERSION,
        "release": {
            "tenant-id": fixture.tenant,
            "catalog-id": fixture.catalog,
            // The bind gate reads only the registration projection, so any
            // non-zero version serves; 1 is what the seeded catalog heads carry.
            "catalog-version": 1,
            "environment": fixture.environment,
        },
        "components": [],
        "wirings": [{
            "wiring-id": fixture.wiring,
            "wiring-version": 1,
            "graph-hash": "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        }],
        "attachments": {},
        "registrations": {
            fixture.registration: {
                "wiring-id": fixture.wiring,
                "wiring-version": 1,
                "entity": fixture.entity,
                "ops": ops,
            },
        },
    });

    let root = std::env::temp_dir().join(format!(
        "wamn-release-manifest-{}-{}",
        std::process::id(),
        fixture.registration
    ));
    std::fs::create_dir_all(&root)
        .with_context(|| format!("create manifest mount {}", root.display()))?;
    std::fs::write(
        root.join(wamn_catalog::RELEASE_MANIFEST_FILE_NAME),
        wamn_flow::canonical_json_bytes(&document),
    )
    .context("write serving manifest")?;
    let loaded = ReleaseManifestWeld::load_from(&root);
    // The weld holds the parsed document for the life of the process and never
    // re-reads the mount, so the gate leaves no manifest behind.
    let _ = std::fs::remove_dir_all(&root);
    Ok(Arc::new(loaded?))
}
