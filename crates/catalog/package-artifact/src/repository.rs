//! Tag enumeration and deletion through the shared registry connection.

use anyhow::ensure;

use crate::Repository;

impl Repository {
    /// List every tag, following the registry's pagination.
    pub async fn tags(&self) -> anyhow::Result<Vec<String>> {
        let mut tags = Vec::new();
        loop {
            let page = self
                .client
                .list_tags(
                    &self.reference,
                    &self.auth,
                    Some(100),
                    tags.last().map(String::as_str),
                )
                .await?;
            if page.tags.is_empty() {
                return Ok(tags);
            }
            ensure!(
                tags.last()
                    .is_none_or(|last| page.tags.last().is_some_and(|next| next > last)),
                "registry tag pagination did not advance"
            );
            tags.extend(page.tags);
        }
    }

    /// Delete an immutable manifest digest. The caller must protect all of its tag references.
    pub async fn delete_manifest(&self, digest: &str) -> anyhow::Result<()> {
        self.source
            .delete_manifest(&self.reference, &self.auth, digest)
            .await
    }
}
