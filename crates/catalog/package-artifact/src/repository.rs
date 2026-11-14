//! Tag enumeration and deletion through the shared registry connection.

use anyhow::{Context as _, ensure};
use oci_client::secrets::RegistryAuth;

use crate::{REGISTRY_IO_TIMEOUT, Repository, ensure_tag};

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

    /// Delete one tag in this repository without deleting its digest or other tags.
    /// Registries that do not support tag deletion refuse the request.
    pub async fn delete_tag(&self, tag: &str) -> anyhow::Result<()> {
        ensure_tag(tag)?;
        tokio::time::timeout(REGISTRY_IO_TIMEOUT, async {
            let url = format!(
                "{}://{}/v2/{}/manifests/{tag}",
                self.scheme,
                self.reference.resolve_registry(),
                self.reference.repository()
            );
            let mut response = self.authenticate(self.http.delete(&url)).send().await?;
            if response.status() == reqwest::StatusCode::UNAUTHORIZED {
                let challenge = response
                    .headers()
                    .get(reqwest::header::WWW_AUTHENTICATE)
                    .context("registry omitted its delete authorization challenge")?
                    .to_str()?;
                let challenge = http_auth::parser::ChallengeParser::new(challenge)
                    .filter_map(Result::ok)
                    .find(|challenge| challenge.scheme.eq_ignore_ascii_case("Bearer"))
                    .context("registry did not offer Bearer authorization for deletion")?;
                let mut realm = None;
                let mut service = None;
                for (name, value) in &challenge.params {
                    if name.eq_ignore_ascii_case("realm") {
                        realm = Some(value.to_unescaped());
                    }
                    if name.eq_ignore_ascii_case("service") {
                        service = Some(value.to_unescaped());
                    }
                }
                let mut realm = reqwest::Url::parse(
                    &realm.context("registry delete authorization has no realm")?,
                )?;
                ensure!(
                    realm.scheme() == "https" || self.scheme == "http",
                    "registry authorization requires HTTPS"
                );
                realm.query_pairs_mut().append_pair(
                    "scope",
                    &format!("repository:{}:delete", self.reference.repository()),
                );
                if let Some(service) = service {
                    realm.query_pairs_mut().append_pair("service", &service);
                }
                let token = self
                    .authenticate(self.http.get(realm))
                    .send()
                    .await?
                    .error_for_status()?;
                let token: serde_json::Value = serde_json::from_slice(&token.bytes().await?)?;
                let token = token
                    .get("token")
                    .or_else(|| token.get("access_token"))
                    .and_then(serde_json::Value::as_str)
                    .context("registry delete authorization has no token")?;
                response = self.http.delete(&url).bearer_auth(token).send().await?;
            }
            ensure!(
                matches!(
                    response.status(),
                    reqwest::StatusCode::ACCEPTED | reqwest::StatusCode::NOT_FOUND
                ),
                "registry refused tag deletion for {}:{tag}: {}",
                self.reference.repository(),
                response.status()
            );
            Ok(())
        })
        .await
        .context("registry tag deletion deadline exceeded")?
    }

    fn authenticate(&self, request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match &self.auth {
            RegistryAuth::Anonymous => request,
            RegistryAuth::Basic(username, password) => request.basic_auth(username, Some(password)),
            RegistryAuth::Bearer(token) => request.bearer_auth(token),
        }
    }
}
