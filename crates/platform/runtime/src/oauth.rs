//! One OAuth protocol implementation over the caller's pinned HTTP transport.

use oauth2::{
    AuthType, AuthUrl, AuthorizationCode, ClientId, ClientSecret, CsrfToken, EndpointNotSet,
    EndpointSet, PkceCodeChallenge, PkceCodeVerifier, RedirectUrl, RefreshToken, Scope,
    TokenResponse as _, TokenUrl,
    basic::{BasicClient, BasicTokenType},
};
use serde::Deserialize;

use crate::plugins::connection_http::ConnectionError;

// The endpoint is required before any exchange can be built.
type Client =
    BasicClient<EndpointNotSet, EndpointNotSet, EndpointNotSet, EndpointNotSet, EndpointSet>;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Parameters {
    pub client_id: String,
    pub token_url: String,
    pub token_authority: Option<String>,
    pub scopes: Vec<String>,
    pub authorize_url: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Credential {
    client_secret: String,
}
impl Drop for Credential {
    fn drop(&mut self) {
        use zeroize::Zeroize as _;
        self.client_secret.zeroize();
    }
}

pub(crate) struct Response {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_in: Option<u64>,
}

enum Grant {
    Credentials {
        scope: Option<String>,
        assertion: Option<String>,
    },
    Refresh(RefreshToken),
    Code {
        code: AuthorizationCode,
        verifier: PkceCodeVerifier,
    },
}

pub(crate) struct Request {
    pub token_url: String,
    pub token_authority: Option<String>,
    client: Client,
    grant: Grant,
}

fn unavailable() -> ConnectionError {
    ConnectionError::CredentialUnavailable
}

fn client(id: String, url: String) -> Result<Client, ConnectionError> {
    if id.is_empty() {
        return Err(unavailable());
    }
    Ok(BasicClient::new(ClientId::new(id))
        .set_token_uri(TokenUrl::new(url).map_err(|_| unavailable())?))
}

fn credential(
    parameters: &serde_json::Value,
    material: Option<&str>,
) -> Result<(Parameters, Client), ConnectionError> {
    let parameters: Parameters =
        serde_json::from_value(parameters.clone()).map_err(|_| unavailable())?;
    let mut secret: Credential =
        serde_json::from_str(material.ok_or_else(unavailable)?).map_err(|_| unavailable())?;
    if secret.client_secret.is_empty() {
        return Err(unavailable());
    }
    let client = client(parameters.client_id.clone(), parameters.token_url.clone())?
        .set_client_secret(ClientSecret::new(std::mem::take(&mut secret.client_secret)));
    Ok((parameters, client))
}

pub(crate) struct Transport {
    pub http: crate::plugins::connection_http::transport::HttpTransport,
    pub decision: crate::connection_authority::AuthorityDecision,
    pub identity: Option<crate::plugins::connection_http::transport::ClientIdentity>,
}

impl<'c> oauth2::AsyncHttpClient<'c> for Transport {
    type Error = crate::plugins::connection_http::transport::TransportError;
    type Future = std::pin::Pin<
        Box<dyn Future<Output = Result<oauth2::HttpResponse, Self::Error>> + Send + 'c>,
    >;
    fn call(&'c self, request: oauth2::HttpRequest) -> Self::Future {
        Box::pin(async move {
            let (parts, body) = request.into_parts();
            let request = hyper::Request::from_parts(parts, body.as_slice());
            let response = match &self.identity {
                Some(identity) => {
                    self.http
                        .execute(identity.clone(), &self.decision, request)
                        .await?
                }
                None => self.http.send_once(&self.decision, request).await?,
            };
            Ok(response.map(|bytes| bytes.to_vec()))
        })
    }
}

impl Request {
    pub(crate) fn credentials(
        auth_type: &str,
        parameters: &serde_json::Value,
        material: Option<&str>,
        refresh: Option<&str>,
    ) -> Result<Self, ConnectionError> {
        let (parameters, client) = credential(parameters, material)?;
        if auth_type == "oauth2_client_credentials" && parameters.authorize_url.is_some() {
            return Err(unavailable());
        }
        let grant = match refresh {
            Some(token) => Grant::Refresh(RefreshToken::new(token.to_owned())),
            None => Grant::Credentials {
                scope: (!parameters.scopes.is_empty()).then(|| parameters.scopes.join(" ")),
                assertion: None,
            },
        };
        Ok(Self {
            token_url: parameters.token_url,
            token_authority: parameters.token_authority,
            client,
            grant,
        })
    }

    pub(crate) fn assertion(
        id: String,
        url: String,
        authority: Option<String>,
        scope: Option<String>,
        assertion: String,
    ) -> Result<Self, ConnectionError> {
        Ok(Self {
            client: client(id, url.clone())?.set_auth_type(AuthType::RequestBody),
            token_url: url,
            token_authority: authority,
            grant: Grant::Credentials {
                scope,
                assertion: Some(assertion),
            },
        })
    }

    pub(crate) fn code(
        parameters: &serde_json::Value,
        material: &str,
        redirect: &str,
        code: String,
        verifier: String,
    ) -> Result<Self, ConnectionError> {
        let (parameters, client) = credential(parameters, Some(material))?;
        Ok(Self {
            token_url: parameters.token_url,
            token_authority: parameters.token_authority,
            client: client
                .set_auth_type(AuthType::RequestBody)
                .set_redirect_uri(
                    RedirectUrl::new(redirect.to_owned()).map_err(|_| unavailable())?,
                ),
            grant: Grant::Code {
                code: AuthorizationCode::new(code),
                verifier: PkceCodeVerifier::new(verifier),
            },
        })
    }

    pub(crate) async fn send(&self, transport: &Transport) -> Result<Response, ConnectionError> {
        self.exchange(transport).await
    }

    pub(crate) async fn exchange<'a, C>(&'a self, send: &'a C) -> Result<Response, ConnectionError>
    where
        C: oauth2::AsyncHttpClient<'a> + Sync,
        C::Future: Send,
    {
        let token = match &self.grant {
            Grant::Credentials { scope, assertion } => {
                let mut request = self.client.exchange_client_credentials();
                if let Some(scope) = scope {
                    request = request.add_extra_param("scope", scope);
                }
                if let Some(assertion) = assertion {
                    request = request
                        .add_extra_param(
                            "client_assertion_type",
                            "urn:ietf:params:oauth:client-assertion-type:jwt-bearer",
                        )
                        .add_extra_param("client_assertion", assertion);
                }
                request.request_async(send).await
            }
            Grant::Refresh(token) => {
                self.client
                    .exchange_refresh_token(token)
                    .request_async(send)
                    .await
            }
            Grant::Code { code, verifier } => {
                self.client
                    .exchange_code(code.clone())
                    .set_pkce_verifier(PkceCodeVerifier::new(verifier.secret().to_owned()))
                    .request_async(send)
                    .await
            }
        }
        .map_err(|_| unavailable())?;
        let access = token.access_token().secret();
        if token.token_type() != &BasicTokenType::Bearer
            || access.is_empty()
            || !access
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"-._~+/=".contains(&byte))
        {
            return Err(unavailable());
        }
        Ok(Response {
            access_token: access.to_owned(),
            refresh_token: token.refresh_token().map(|token| token.secret().to_owned()),
            expires_in: token.expires_in().map(|duration| duration.as_secs()),
        })
    }
}

/// Build state, PKCE and the provider URL together.
pub(crate) fn authorize(
    parameters: &serde_json::Value,
    redirect: &str,
) -> Result<(String, String, String), ConnectionError> {
    let auth: Parameters = serde_json::from_value(parameters.clone()).map_err(|_| unavailable())?;
    let url =
        AuthUrl::new(auth.authorize_url.ok_or_else(unavailable)?).map_err(|_| unavailable())?;
    if !matches!(url.url().scheme(), "http" | "https")
        || !url.url().username().is_empty()
        || url.url().password().is_some()
        || url.url().fragment().is_some()
    {
        return Err(unavailable());
    }
    let client = client(auth.client_id, auth.token_url)?
        .set_auth_uri(url)
        .set_redirect_uri(RedirectUrl::new(redirect.to_owned()).map_err(|_| unavailable())?);
    let (challenge, verifier) = PkceCodeChallenge::new_random_sha256();
    let (url, state) = client
        .authorize_url(|| CsrfToken::new_random_len(32))
        .add_scopes(auth.scopes.into_iter().map(Scope::new))
        .set_pkce_challenge(challenge)
        .url();
    Ok((
        url.into(),
        state.secret().to_owned(),
        verifier.secret().to_owned(),
    ))
}
