//! Object transfers for create-only publication.

use anyhow::{Context as _, ensure};
use object_store::aws::AmazonS3Builder;
use object_store::gcp::GoogleCloudStorageBuilder;
use object_store::{Attribute, Attributes, ObjectStore, ObjectStoreExt as _, PutMode, PutOptions};

/// Open a bucket with the endpoint and credentials supplied by `AWS_*` variables.
pub fn s3(bucket: &str) -> anyhow::Result<Box<dyn ObjectStore>> {
    Ok(Box::new(
        AmazonS3Builder::from_env()
            .with_bucket_name(bucket)
            .build()
            .context("configure the bucket from the AWS_* variables")?,
    ))
}

/// Open a bucket with `GOOGLE_*` variables and Application Default Credentials.
pub fn gcs(bucket: &str) -> anyhow::Result<Box<dyn ObjectStore>> {
    Ok(Box::new(
        GoogleCloudStorageBuilder::from_env()
            .with_bucket_name(bucket)
            .build()
            .context(
                "configure the bucket from the GOOGLE_* variables and \
                 Application Default Credentials",
            )?,
    ))
}

/// Look for an object without reading its body.
pub async fn exists(store: &dyn ObjectStore, key: &str) -> object_store::Result<bool> {
    match store.head(&key.into()).await {
        Ok(_) => Ok(true),
        Err(object_store::Error::NotFound { .. }) => Ok(false),
        Err(error) => Err(error),
    }
}

/// Write one object create-only with its content type. An
/// existing object with equal bytes is the write of an earlier run that
/// stopped, so it stays. An existing object with other bytes refuses.
pub async fn put(
    store: &dyn ObjectStore,
    object_key: &str,
    bytes: Vec<u8>,
    content_type: &'static str,
) -> anyhow::Result<()> {
    let mut attributes = Attributes::new();
    attributes.insert(Attribute::ContentType, content_type.into());
    let key = object_store::path::Path::from(object_key);
    let bytes = object_store::PutPayload::from(bytes);
    let written = store
        .put_opts(
            &key,
            bytes.clone(),
            PutOptions {
                mode: PutMode::Create,
                attributes,
                ..PutOptions::default()
            },
        )
        .await;
    match written {
        Err(
            object_store::Error::AlreadyExists { .. } | object_store::Error::Precondition { .. },
        ) => {
            let existing = store
                .get(&key)
                .await
                .with_context(|| format!("read the existing {key}"))?
                .bytes()
                .await
                .with_context(|| format!("read the existing {key}"))?;
            ensure!(
                existing
                    .as_ref()
                    .iter()
                    .copied()
                    .eq(bytes.iter().flat_map(|part| part.iter().copied())),
                "write {key}; an existing object with other bytes is never replaced"
            );
            Ok(())
        }
        written => written
            .map(|_| ())
            .with_context(|| format!("write {key}; an existing object is never replaced")),
    }
}
