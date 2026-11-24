//! One OAuth protocol implementation over the caller's pinned HTTP transport.

use oauth2::{
    AuthType, AuthUrl, AuthorizationCode, ClientId, ClientSecret, CsrfToken, EndpointNotSet,
    EndpointSet, PkceCodeChallenge, PkceCodeVerifier, RedirectUrl, RefreshToken, Scope,
    TokenResponse as _, TokenUrl,
    basic::{BasicClient, BasicTokenType},
};
use serde::Deserialize;

use wamn_authority::connection::ConnectionError;
use wamn_authority::connection_authority::parse_http_definition;
use wamn_connection::oauth_token::{Fetched, Token};

use crate::transport::HttpTransport;

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

pub(crate) struct Transport<'a> {
    pub http: &'a HttpTransport,
    pub decision: wamn_authority::connection_authority::AuthorityDecision,
}

impl<'c> oauth2::AsyncHttpClient<'c> for Transport<'_> {
    type Error = crate::transport::TransportError;
    type Future = std::pin::Pin<
        Box<dyn Future<Output = Result<oauth2::HttpResponse, Self::Error>> + Send + 'c>,
    >;
    fn call(&'c self, request: oauth2::HttpRequest) -> Self::Future {
        Box::pin(async move {
            let (parts, body) = request.into_parts();
            let request = hyper::Request::from_parts(parts, body.as_slice());
            let response = self.http.send_once(&self.decision, request).await?;
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

    pub(crate) async fn send(
        &self,
        transport: &Transport<'_>,
    ) -> Result<Response, ConnectionError> {
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
pub fn authorize(
    parameters: &serde_json::Value,
    redirect: &str,
    scope: &str,
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
        .authorize_url(|| {
            CsrfToken::new(format!(
                "{scope}/{}",
                CsrfToken::new_random_len(32).secret()
            ))
        })
        .add_scopes(auth.scopes.into_iter().map(Scope::new))
        .set_pkce_challenge(challenge)
        .url();
    Ok((
        url.into(),
        state.secret().to_owned(),
        verifier.secret().to_owned(),
    ))
}

/// A failed exchange with a reason safe to show on the callback page.
#[derive(Debug)]
pub struct CodeExchangeError {
    pub reason: &'static str,
    source: anyhow::Error,
}

impl std::fmt::Display for CodeExchangeError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.reason)
    }
}

impl std::error::Error for CodeExchangeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.source.as_ref())
    }
}

/// Exchange a code at the token endpoint bound to the connection's authority.
pub async fn exchange_code(
    transport: &HttpTransport,
    parameters: &serde_json::Value,
    material: &str,
    redirect: &str,
    code: String,
    verifier: String,
) -> Result<Fetched, CodeExchangeError> {
    let request = Request::code(&parameters["auth"], material, redirect, code, verifier).map_err(
        |source| CodeExchangeError {
            reason: "invalid authorization parameters",
            source: source.into(),
        },
    )?;
    let authority = parse_http_definition(parameters).map_err(|source| CodeExchangeError {
        reason: "invalid connection authority",
        source: source.into(),
    })?;
    let decision = crate::token_endpoint(
        &authority,
        request.token_authority.as_deref(),
        &request.token_url,
        transport,
    )
    .await
    .map_err(|source| CodeExchangeError {
        reason: "token endpoint authority refused",
        source: source.into(),
    })?;
    let token = crate::fetch_health_token(transport, decision, &request)
        .await
        .map_err(|source| CodeExchangeError {
            reason: "invalid provider token response",
            source: source.into(),
        })?;
    Ok(Fetched {
        token: Token {
            access_token: Some(token.access_token),
            refresh_token: token.refresh_token,
        },
        expires_in: token.expires_in,
    })
}
