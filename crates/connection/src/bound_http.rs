//! Host-supplied HTTP for a resolved connection and one credential generation.

use std::fmt;
use std::sync::Arc;

use wamn_application::InvocationLifetime;
use wamn_authority::connection::ConnectionError;
use wamn_identity_protocol::PrincipalId;

use crate::connection_facts::{ConnectionAnswer, ConnectionFacts};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Target {
    Cloud,
    Local,
}

impl Target {
    pub fn from_target_type(target: &str) -> Option<Self> {
        match target {
            "cloud" => Some(Self::Cloud),
            "local" => Some(Self::Local),
            _ => None,
        }
    }
}

pub struct Header {
    pub name: String,
    pub value: Vec<u8>,
}

pub struct Request {
    pub method: String,
    pub path_and_query: String,
    pub headers: Vec<Header>,
    pub body: Option<Vec<u8>>,
    pub idempotency_key: Option<String>,
}

pub struct Response {
    pub external_system: Option<String>,
    pub status: u16,
    pub headers: Vec<Header>,
    pub body: Vec<u8>,
}

pub struct Call<'a> {
    pub facts: &'a ConnectionFacts,
    pub connection: uuid::Uuid,
    pub target: Target,
    pub execution: &'a Arc<dyn InvocationLifetime>,
    pub request: &'a Request,
    pub principal: &'a (dyn Fn() -> Option<PrincipalId> + Sync),
}

#[derive(Clone, Copy)]
pub struct Attempt<'a> {
    pub answer: &'a ConnectionAnswer,
    pub material: Option<&'a str>,
    pub rejected: Option<&'a str>,
}

/// Exchange one already-bound connection request using the supplied generation.
#[async_trait::async_trait]
pub trait BoundHttp: fmt::Debug + Send + Sync {
    async fn attempt(
        &self,
        call: &Call<'_>,
        attempt: Attempt<'_>,
    ) -> Result<(Response, Option<String>), ConnectionError>;
}

impl fmt::Debug for Header {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Header")
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}

impl fmt::Debug for Request {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Request")
            .field("method", &self.method)
            .finish_non_exhaustive()
    }
}

impl fmt::Debug for Response {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Response")
            .field("status", &self.status)
            .finish_non_exhaustive()
    }
}

impl fmt::Debug for Call<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Call")
            .field("connection", &self.connection)
            .finish_non_exhaustive()
    }
}

impl fmt::Debug for Attempt<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Attempt").finish_non_exhaustive()
    }
}
