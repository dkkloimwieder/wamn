//! Deployment-owned trust and bounded native reuse for reviewed component bytes.

use std::collections::BTreeSet;

use wamn_catalog::ArtifactHash;

/// Trust granted by the deployment owner, never by an application manifest.
///
/// Each digest names reviewed code whose caller data and tasks are request-local.
/// The native runtime also requires every member of a shared store to be eligible.
#[derive(Debug, Clone)]
pub struct WarmReuse {
    trusted: BTreeSet<String>,
    pool_size: i32,
    reclaim_seconds: i32,
}

impl Default for WarmReuse {
    fn default() -> Self {
        Self {
            trusted: BTreeSet::new(),
            pool_size: 1,
            reclaim_seconds: 60,
        }
    }
}

impl WarmReuse {
    /// Read exact trusted digests and positive native limits from deployment inputs.
    pub fn new(trusted: &[String], pool_size: i32, reclaim_seconds: i32) -> anyhow::Result<Self> {
        anyhow::ensure!(pool_size > 0, "component pool size must be positive");
        anyhow::ensure!(
            reclaim_seconds > 0,
            "component reclaim window must be positive"
        );
        let trusted = trusted
            .iter()
            .map(|digest| ArtifactHash::parse(digest.clone()).map(|hash| hash.to_string()))
            .collect::<Result<_, _>>()?;
        Ok(Self {
            trusted,
            pool_size,
            reclaim_seconds,
        })
    }

    pub fn apply(&self, component: &mut wash_runtime::types::Component) {
        if component
            .digest
            .as_ref()
            .is_some_and(|digest| self.trusted.contains(digest))
        {
            component.pool_size = self.pool_size;
            component.max_concurrency = 1;
            // Bound retained guest allocations even when traffic never becomes idle.
            component.max_invocations = 1000;
            component.reclaim_window_seconds = self.reclaim_seconds;
            component.reclaim_min_instances = 0;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::WarmReuse;

    #[test]
    fn deployment_trust_requires_exact_digest_and_bounded_native_configuration() {
        let digest = format!("sha256:{}", "a".repeat(64));
        assert!(WarmReuse::new(&["unreviewed-name".into()], 1, 60).is_err());
        assert!(WarmReuse::new(&[], 0, 60).is_err());
        assert!(WarmReuse::new(&[], 1, 0).is_err());
        let policy =
            WarmReuse::new(std::slice::from_ref(&digest), 2, 30).expect("deployment trust");
        let mut component = wash_runtime::types::Component {
            digest: Some(format!("sha256:{}", "b".repeat(64))),
            ..wash_runtime::types::Component::default()
        };
        policy.apply(&mut component);
        assert_eq!(component.pool_size, 0, "different bytes remain fresh");
        component.digest = Some(digest);
        policy.apply(&mut component);
        assert_eq!(component.pool_size, 2);
        assert_eq!(component.max_concurrency, 1);
        assert_eq!(component.reclaim_window_seconds, 30);
        assert_eq!(component.reclaim_min_instances, 0);
    }
}
