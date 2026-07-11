//! The `apiproof` subcommand: the 4.1b in-cluster proof that the api-gateway
//! component, deployed as a real `WorkloadDeployment`, serves the generated REST
//! API over the network.
//!
//! Where `apibench` drives the gateway in-process (via `ProxyPre`, the exact
//! mechanism wash-runtime uses) to exhaustively prove the SQL/CRUD/RLS
//! *semantics*, `apiproof` proves the *serving path*: it makes real HTTP requests
//! to the deployed gateway's Service — routed by the operator on the `Host`
//! header to the component, run under the host-injected `app.tenant` claim — and
//! asserts the same CRUD / expansion / RLS / injection-safety results end to end.
//! It runs from the same `wamn-host` image (no external client image to pull),
//! mirroring the S4 `nodebench --hop-url` cross-pod gate.
//!
//! The gateway is scoped to `tenant-a` by its workload config; `apiproof`
//! addresses the bundled demo rows by the ids `publish-catalog --seed` wrote
//! (see `apifixture`), so the assertions match `apibench`'s verbatim.

use anyhow::{Context as _, bail};
use clap::{Args, ValueEnum};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::TcpStream;

use crate::apifixture::{S_ACME, S_OTHER, as_array, check, has_name};

#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
pub enum Mode {
    /// list / get / create / update / delete round-trip.
    Crud,
    /// one-level to-one + to-many relation expansion.
    Expand,
    /// a different tenant's rows are invisible (RLS + the injected claim).
    Rls,
    /// injection payloads are parameters; unknown identifiers are rejected.
    Injection,
    /// every gate in sequence.
    All,
}

#[derive(Debug, Args)]
pub struct ApiProofArgs {
    /// TCP base URL of the deployed gateway Service, e.g.
    /// `http://api-gateway.wamn-system.svc.cluster.local:80`.
    #[arg(long)]
    pub url: String,

    /// The `Host` header to route by: the gateway WorkloadDeployment's
    /// `hostInterfaces[].config.host`.
    #[arg(long, default_value = "api.localhost.direct")]
    pub host: String,

    /// Which gate to run.
    #[arg(long, value_enum, default_value_t = Mode::All)]
    pub mode: Mode,
}

/// A minimal HTTP/1.1 client for one request per connection (`Connection: close`)
/// — no client dependency, like the S5 Loki client. Routes by the `Host` header,
/// which is the operator's key, independent of the TCP target.
struct Client {
    host_port: String,
    route_host: String,
}

impl Client {
    fn new(url: &str, route_host: &str) -> anyhow::Result<Self> {
        let host_port = url.strip_prefix("http://").unwrap_or(url);
        // Trim any trailing path; we only need host:port for the socket.
        let host_port = host_port.split('/').next().unwrap_or(host_port);
        Ok(Self {
            host_port: host_port.to_string(),
            route_host: route_host.to_string(),
        })
    }

    /// Send one request and return `(status, json-body)`. A 204/empty body maps
    /// to `Value::Null`.
    async fn request(
        &self,
        method: &str,
        path: &str,
        body: Option<Value>,
    ) -> anyhow::Result<(u16, Value)> {
        let (host, port) = match self.host_port.split_once(':') {
            Some((h, p)) => (h.to_string(), p.parse::<u16>().unwrap_or(80)),
            None => (self.host_port.clone(), 80),
        };
        let body_bytes = match &body {
            Some(v) => serde_json::to_vec(v)?,
            None => Vec::new(),
        };
        let mut req = format!(
            "{method} {path} HTTP/1.1\r\nHost: {}\r\nAccept: application/json\r\nConnection: close\r\n",
            self.route_host
        );
        if body.is_some() {
            req.push_str("Content-Type: application/json\r\n");
            req.push_str(&format!("Content-Length: {}\r\n", body_bytes.len()));
        }
        req.push_str("\r\n");

        let raw = tokio::time::timeout(std::time::Duration::from_secs(15), async {
            let mut stream = TcpStream::connect((host.as_str(), port))
                .await
                .with_context(|| format!("connect {host}:{port}"))?;
            stream.set_nodelay(true)?;
            stream.write_all(req.as_bytes()).await?;
            if !body_bytes.is_empty() {
                stream.write_all(&body_bytes).await?;
            }
            stream.flush().await?;
            let mut buf = Vec::new();
            stream.read_to_end(&mut buf).await?;
            anyhow::Ok(buf)
        })
        .await
        .context("request timed out")??;

        let text = String::from_utf8_lossy(&raw);
        let status = parse_status(&text)
            .with_context(|| format!("no status line in response to {method} {path}"))?;
        let (headers, body_str) = text
            .split_once("\r\n\r\n")
            .map(|(h, b)| (h.to_string(), b.to_string()))
            .unwrap_or_default();
        let body_str = if headers
            .to_ascii_lowercase()
            .contains("transfer-encoding: chunked")
        {
            dechunk(&body_str)
        } else {
            body_str
        };
        let json = if body_str.trim().is_empty() {
            Value::Null
        } else {
            serde_json::from_str(&body_str).unwrap_or(Value::Null)
        };
        Ok((status, json))
    }
}

/// Parse the status code out of `HTTP/1.1 <code> <reason>`.
fn parse_status(text: &str) -> Option<u16> {
    let line = text.lines().next()?;
    line.split_whitespace().nth(1)?.parse().ok()
}

/// Decode an HTTP/1.1 chunked body: `<hex-size>\r\n<data>\r\n` … `0\r\n\r\n`.
fn dechunk(body: &str) -> String {
    let mut out = String::new();
    let mut rest = body;
    while let Some((size_line, after)) = rest.split_once("\r\n") {
        let size = usize::from_str_radix(size_line.trim().split(';').next().unwrap_or("0"), 16)
            .unwrap_or(0);
        if size == 0 || after.len() < size {
            if size != 0 {
                out.push_str(&after[..after.len().min(size)]);
            }
            break;
        }
        out.push_str(&after[..size]);
        rest = after[size..].strip_prefix("\r\n").unwrap_or(&after[size..]);
    }
    out
}

// ---- gates ----------------------------------------------------------------

async fn crud_phase(c: &Client) -> anyhow::Result<bool> {
    println!("\n## crud");
    let mut ok = true;

    let (s, body) = c.request("GET", "/api/rest/suppliers", None).await?;
    let rows = as_array(&body);
    check(
        &mut ok,
        "list suppliers -> 200 with the tenant's two rows",
        s == 200 && rows.len() == 2 && has_name(&rows, "Acme") && has_name(&rows, "Globex"),
    );

    let (s, body) = c
        .request("GET", &format!("/api/rest/suppliers/{S_ACME}"), None)
        .await?;
    check(
        &mut ok,
        "get by id -> 200, numeric is an exact-decimal string",
        s == 200
            && body.get("name").and_then(Value::as_str) == Some("Acme")
            && body.get("standard_cost").and_then(Value::as_str) == Some("12.50"),
    );

    let (s, body) = c
        .request("GET", "/api/rest/suppliers?standard_cost=eq.99.99", None)
        .await?;
    let rows = as_array(&body);
    check(
        &mut ok,
        "filter standard_cost=eq.99.99 -> 200, one match",
        s == 200 && rows.len() == 1 && has_name(&rows, "Globex"),
    );

    let (s, created) = c
        .request(
            "POST",
            "/api/rest/suppliers",
            Some(json!({ "name": "NewCo", "standard_cost": "7.25" })),
        )
        .await?;
    let new_id = created
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    check(
        &mut ok,
        "create -> 201 with a generated id + the row",
        s == 201
            && !new_id.is_empty()
            && created.get("name").and_then(Value::as_str) == Some("NewCo"),
    );

    let (s, updated) = c
        .request(
            "PATCH",
            &format!("/api/rest/suppliers/{new_id}"),
            Some(json!({ "standard_cost": "8.00" })),
        )
        .await?;
    check(
        &mut ok,
        "update -> 200 with the new value",
        s == 200 && updated.get("standard_cost").and_then(Value::as_str) == Some("8.00"),
    );

    let (s, _) = c
        .request("DELETE", &format!("/api/rest/suppliers/{new_id}"), None)
        .await?;
    check(&mut ok, "delete -> 204", s == 204);

    let (s, _) = c
        .request("GET", &format!("/api/rest/suppliers/{new_id}"), None)
        .await?;
    check(&mut ok, "get deleted -> 404", s == 404);

    Ok(ok)
}

async fn expand_phase(c: &Client) -> anyhow::Result<bool> {
    println!("\n## expand");
    let mut ok = true;

    let (s, body) = c
        .request("GET", "/api/rest/receipts?expand=supplier", None)
        .await?;
    let rows = as_array(&body);
    let supplier_ok = rows
        .first()
        .and_then(|r| r.get("supplier"))
        .and_then(|sup| sup.get("name"))
        .and_then(Value::as_str)
        == Some("Acme");
    check(
        &mut ok,
        "expand=supplier embeds the to-one parent (Acme)",
        s == 200 && supplier_ok,
    );

    let (s, body) = c
        .request("GET", "/api/rest/receipts?expand=lines", None)
        .await?;
    let rows = as_array(&body);
    let lines = rows
        .first()
        .and_then(|r| r.get("lines"))
        .map(as_array)
        .unwrap_or_default();
    let qty_ok = lines
        .iter()
        .any(|l| l.get("quantity").and_then(Value::as_str) == Some("3.000"))
        && lines
            .iter()
            .any(|l| l.get("quantity").and_then(Value::as_str) == Some("5.500"));
    check(
        &mut ok,
        "expand=lines embeds the to-many child array (2 lines, exact-decimal)",
        s == 200 && lines.len() == 2 && qty_ok,
    );

    Ok(ok)
}

async fn rls_phase(c: &Client) -> anyhow::Result<bool> {
    println!("\n## rls");
    let mut ok = true;

    let (s, body) = c.request("GET", "/api/rest/suppliers", None).await?;
    let rows = as_array(&body);
    check(
        &mut ok,
        "the other tenant's row is invisible (RLS + app.tenant)",
        s == 200 && !has_name(&rows, "OtherTenantCo"),
    );

    let (s, _) = c
        .request("GET", &format!("/api/rest/suppliers/{S_OTHER}"), None)
        .await?;
    check(&mut ok, "get another tenant's row by id -> 404", s == 404);

    Ok(ok)
}

async fn injection_phase(c: &Client) -> anyhow::Result<bool> {
    println!("\n## injection");
    let mut ok = true;

    let evil = "/api/rest/suppliers?name=eq.Acme%27%3B%20DROP%20TABLE%20suppliers%3B%20--";
    let (s, body) = c.request("GET", evil, None).await?;
    check(
        &mut ok,
        "injection filter value -> 200, zero matches (bound as a param)",
        s == 200 && as_array(&body).is_empty(),
    );

    let (s, body) = c.request("GET", "/api/rest/suppliers", None).await?;
    check(
        &mut ok,
        "the suppliers table survived the injection attempt",
        s == 200 && !as_array(&body).is_empty(),
    );

    let (s, _) = c.request("GET", "/api/rest/nonexistent", None).await?;
    check(
        &mut ok,
        "unknown entity -> 400 (rejected before any SQL)",
        s == 400,
    );

    let (s, _) = c
        .request("GET", "/api/rest/suppliers?bogus_col=1", None)
        .await?;
    check(&mut ok, "unknown filter column -> 400", s == 400);

    Ok(ok)
}

pub async fn run(args: ApiProofArgs) -> anyhow::Result<()> {
    println!("# wamn-host 4.1b apiproof");
    println!("target {} (Host: {})", args.url, args.host);

    let client = Client::new(&args.url, &args.host)?;
    let run_all = args.mode == Mode::All;
    let mut pass = true;

    if run_all || args.mode == Mode::Crud {
        pass &= crud_phase(&client).await?;
    }
    if run_all || args.mode == Mode::Expand {
        pass &= expand_phase(&client).await?;
    }
    if run_all || args.mode == Mode::Rls {
        pass &= rls_phase(&client).await?;
    }
    if run_all || args.mode == Mode::Injection {
        pass &= injection_phase(&client).await?;
    }

    println!("\napiproof complete — overall PASS: {pass}");
    if !pass {
        bail!("4.1b apiproof gate failed");
    }
    Ok(())
}
