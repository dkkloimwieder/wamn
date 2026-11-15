//! Tag enumeration and deletion through the shared registry connection.

use crate::Repository;

impl Repository {
    /// List every tag, following the registry's pagination.
    pub async fn tags(&self) -> anyhow::Result<Vec<crate::tag::OciTag>> {
        self.source
            .tags(&self.reference, &self.auth)
            .await?
            .into_iter()
            .map(|tag| Ok(tag.try_into()?))
            .collect()
    }

    /// Delete an immutable manifest digest. The caller must protect all of its tag references.
    pub async fn delete_manifest(&self, digest: &wamn_digest::Digest) -> anyhow::Result<()> {
        self.source
            .delete_manifest(&self.reference, &self.auth, digest)
            .await
    }
}
