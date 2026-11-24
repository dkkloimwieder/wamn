//! S3 and GCS client construction and health checks.

use std::sync::Arc;

use object_store::aws::AmazonS3Builder;
use object_store::gcp::GoogleCloudStorageBuilder;
use object_store::{ClientOptions, RetryConfig};
use wamn_connection_type::{BlobstoreDefinition, BlobstoreProvider};

use crate::store::{ObjectSigner, Objects};

/// Object operations and signing for a bucket. The caller holds binding authority.
#[derive(Debug)]
pub struct StoreClient {
    pub store: Objects,
    pub signer: ObjectSigner,
}

/// Open an S3 bucket with credentials from the connection.
pub fn s3(endpoint: &str, container: &str, secret: &str) -> anyhow::Result<StoreClient> {
    let client = build_s3_store(endpoint, container, secret)?;
    let store = Arc::clone(&client);
    Ok(StoreClient {
        store: Objects::new(store),
        signer: ObjectSigner::new(client),
    })
}

/// Open a GCS bucket with pod credentials.
pub fn gcs(container: &str) -> anyhow::Result<StoreClient> {
    let client = build_gcs_store(container)?;
    let store = Arc::clone(&client);
    Ok(StoreClient {
        store: Objects::new(store),
        signer: ObjectSigner::new(client),
    })
}

/// Resolve the storage provider for a configured integration.
pub fn provider(
    integration_type: &wamn_registry::integration_code::IntegrationCode,
) -> anyhow::Result<BlobstoreProvider> {
    match integration_type.as_str() {
        "s3" => Ok(BlobstoreProvider::S3),
        "gcs" => Ok(BlobstoreProvider::Gcs),
        _ => anyhow::bail!("not a storage integration"),
    }
}

/// Build the S3 client for one binding.
///
/// The secret enters HERE and nowhere else: it is handed straight to the
/// signer and stays out of diagnostic output and guest-visible structures.
fn build_s3_store(
    endpoint: &str,
    container: &str,
    secret: &str,
) -> anyhow::Result<Arc<object_store::aws::AmazonS3>> {
    #[derive(serde::Deserialize)]
    struct Credential {
        #[serde(rename = "ACCESS_KEY_ID")]
        access_key: String,
        #[serde(rename = "ACCESS_SECRET_KEY")]
        secret_key: String,
    }
    let credential: Credential = serde_json::from_str(secret)?;
    let store = AmazonS3Builder::new()
        .with_endpoint(endpoint)
        .with_bucket_name(container)
        .with_access_key_id(credential.access_key)
        .with_secret_access_key(credential.secret_key)
        .with_region("us-east-1")
        .with_virtual_hosted_style_request(false)
        .with_client_options(
            ClientOptions::new()
                .with_allow_http(endpoint.starts_with("http://"))
                .with_timeout(wamn_application::MAX_HOST_CALL_DURATION),
        )
        .with_retry(RetryConfig {
            retry_timeout: wamn_application::MAX_HOST_CALL_DURATION,
            ..Default::default()
        })
        .build()?;
    Ok(Arc::new(store))
}

/// Build Cloud Storage with pod metadata credentials and IAM signing.
/// Explicit providers prevent fallback to stored ADC keys.
fn build_gcs_store(container: &str) -> anyhow::Result<Arc<object_store::gcp::GoogleCloudStorage>> {
    let (credentials, signing_credentials) = crate::pod_identity::providers()?;
    let store = GoogleCloudStorageBuilder::new()
        .with_credentials(credentials)
        .with_signing_credentials(signing_credentials)
        .with_bucket_name(container)
        .with_client_options(
            ClientOptions::new().with_timeout(wamn_application::MAX_HOST_CALL_DURATION),
        )
        .with_retry(RetryConfig {
            retry_timeout: wamn_application::MAX_HOST_CALL_DURATION,
            ..Default::default()
        })
        .build()?;
    Ok(Arc::new(store))
}

/// List the first object under candidate storage coordinates without writing facts.
pub async fn health(
    integration_type: &wamn_registry::integration_code::IntegrationCode,
    parameters: &serde_json::Value,
    credential: Option<&str>,
) -> anyhow::Result<()> {
    use object_store::list::{PaginatedListOptions, PaginatedListStore};

    let bound = BlobstoreDefinition::parse_for(parameters, provider(integration_type)?)?;
    let prefix = object_store::path::Path::parse(&bound.prefix)?;
    let store: Arc<dyn PaginatedListStore> = match bound.provider {
        BlobstoreProvider::Gcs => build_gcs_store(&bound.container)?,
        BlobstoreProvider::S3 => build_s3_store(
            bound.endpoint.as_deref().expect("validated s3 endpoint"),
            &bound.container,
            credential.ok_or_else(|| anyhow::anyhow!("storage credential unavailable"))?,
        )?,
    };
    let prefix = format!("{}/", prefix.as_ref().trim_end_matches('/'));
    store
        .list_paginated(
            Some(&prefix),
            PaginatedListOptions {
                max_keys: Some(1),
                ..PaginatedListOptions::default()
            },
        )
        .await?;
    Ok(())
}
