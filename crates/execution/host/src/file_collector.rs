//! Collect aged file objects through the host's scoped readers.

use std::time::{Duration, SystemTime};

use futures_util::{StreamExt as _, TryStreamExt as _};

use crate::file_route::{FileConnection, FileRoutes};

/// Run collection once an hour within the existing host refresh task.
pub const COLLECTION_INTERVAL: Duration = Duration::from_hours(1);
/// Keep a one-day safety window, as in the C4 staging lifecycle test.
/// Upload commits metadata before promotion, so no live object precedes its row.
pub const MIN_OBJECT_AGE: Duration = Duration::from_hours(24);
const BATCH_SIZE: usize = 100;

// Different generations or connections can still name the same physical objects.
// CDN origins and credentials do not change that namespace.
fn namespace(connection: &FileConnection) -> (String, String, String, String) {
    let field = |key: &str| {
        connection.definition[key]
            .as_str()
            .unwrap_or_default()
            .to_owned()
    };
    let provider = connection.definition["provider"]
        .as_str()
        .unwrap_or("s3")
        .to_owned();
    let endpoint = if provider == "gcs" {
        String::new()
    } else {
        let endpoint = field("endpoint");
        url::Url::parse(&endpoint)
            .map_or(endpoint.clone(), |url| url.to_string())
            .trim_end_matches('/')
            .to_owned()
    };
    (
        provider,
        endpoint,
        field("container"),
        field("prefix")
            .split('/')
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join("/"),
    )
}

impl FileRoutes {
    /// Collect old canonical live objects with no exact row in their physical store.
    /// Listing stays within this environment's segment, including shared stores.
    /// Failed reads refuse deletion. Each store failure is logged and other stores continue.
    pub async fn collect_orphan_files(&self) -> anyhow::Result<u64> {
        let Some(reader) = self
            .postgres
            .file_collection(&self.project, &self.release.scope().env_key)
            .await?
        else {
            return Ok(0);
        };
        let cutoff = SystemTime::now() - MIN_OBJECT_AGE;
        let generations = self.connections.generations().await?;
        let mut visited = std::collections::HashSet::new();
        let mut deleted = 0;
        let mut failure = None;
        for connection in generations.iter().rev() {
            let location = namespace(connection);
            if visited.contains(&location) {
                continue;
            }
            let result = async {
                let store = self.blobstore.file_store(&connection.name, false).await?;
                let mut batches = store.live_objects().await?.try_filter(|object| std::future::ready(object.last_modified < cutoff)).try_chunks(BATCH_SIZE);
                while let Some(batch) = batches.next().await {
                    let batch = batch.map_err(|error| error.1)?;
                    let keys: Vec<_> = batch.iter().map(|object| object.blob_key.clone()).collect();
                    let rows = reader.rows(&keys).await?;
                    for object in batch {
                        let present = rows.iter().any(|(id, key)| key == &object.blob_key && generations.iter().find(|pin| &pin.id == id).is_none_or(|pin| namespace(pin) == location));
                        if !present && store.delete_collected(&object, cutoff).await? {
                            deleted += 1;
                            tracing::info!(object_key = object.object_key(), connection_id = %connection.id, generation = connection.generation, "file collector deleted orphan object");
                        }
                    }
                }
                anyhow::Ok(())
            }.await;
            match result {
                Ok(()) => {
                    visited.insert(location);
                }
                Err(error) => {
                    tracing::warn!(%error, connection_id = %connection.id, generation = connection.generation, "file orphan collection failed");
                    failure = Some(error);
                }
            }
        }
        if let Some(error) = failure {
            return Err(error);
        }
        Ok(deleted)
    }
}
