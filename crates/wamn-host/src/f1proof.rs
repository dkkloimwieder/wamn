//! f1proof — the POC-F1 over-the-network proof (the apiproof pattern): drives
//! the DEPLOYED `webhook-entry` WorkloadDeployment through its Service with a
//! hand-rolled HTTP/1.1 client, cross-checks the holds through the DEPLOYED
//! F1 api-gateway instance (generated REST over the same `poc_f1` schema), and
//! audits the write-ahead runs / node_runs / quality_holds directly in the
//! database — the full "end-to-end via catalog API + generated REST" path over
//! real cluster networking. Runs from the wamn-host image; the schema is
//! provisioned beforehand by the `f1-provision` Job (`publish-catalog
//! --provision --runstate --seed-dataset --flow`).

use anyhow::{Context as _, bail};
use clap::{Args, ValueEnum};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::TcpStream;
use tokio_postgres::NoTls;

use crate::apifixture::{as_array, check};
use crate::f1fixture::{BURST_HOLDS, burst, in_spec_receipt, receipt};

#[derive(Debug, Args)]
pub struct F1ProofArgs {
    /// The deployed webhook-entry Service, e.g.
    /// http://webhook-entry.wamn-system.svc.cluster.local:80
    #[arg(long)]
    pub url: String,

    /// Host header routing to the webhook-entry component.
    #[arg(long, default_value = "f1.localhost.direct")]
    pub host: String,

    /// The deployed F1 api-gateway Service (generated REST over poc_f1).
    #[arg(long)]
    pub rest_url: String,

    /// Host header routing to the F1 api-gateway instance.
    #[arg(long, default_value = "api-f1.localhost.direct")]
    pub rest_host: String,

    /// Superuser URL for the DB audit assertions (env `WAMN_PG_ADMIN_URL`).
    #[arg(long, env = "WAMN_PG_ADMIN_URL")]
    pub admin_database_url: Option<String>,

    /// The provisioned project schema.
    #[arg(long, default_value = "poc_f1")]
    pub schema: String,

    #[arg(long, value_enum, default_value_t = Mode::All)]
    pub mode: Mode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Mode {
    Sync,
    Burst,
    Rest,
    All,
}

// ---------------------------------------------------------------------------
// Hand-rolled HTTP/1.1 client (the apiproof shape: raw TcpStream, routing Host
// header decoupled from the TCP target, Connection: close + read_to_end).
// ---------------------------------------------------------------------------

struct Client {
    host_port: String,
    route_host: String,
}

impl Client {
    fn new(url: &str, route_host: &str) -> Client {
        let host_port = url.strip_prefix("http://").unwrap_or(url);
        let host_port = host_port.split('/').next().unwrap_or(host_port);
        Client {
            host_port: host_port.to_string(),
            route_host: route_host.to_string(),
        }
    }

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
            .with_context(|| format!("no status line in response: {:?}", text.get(..80)))?;
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

fn parse_status(text: &str) -> Option<u16> {
    text.lines().next()?.split_whitespace().nth(1)?.parse().ok()
}

fn dechunk(body: &str) -> String {
    let mut out = String::new();
    let mut rest = body;
    while let Some((size_line, after)) = rest.split_once("\r\n") {
        let Ok(size) = usize::from_str_radix(size_line.trim().split(';').next().unwrap_or(""), 16)
        else {
            break;
        };
        if size == 0 {
            break;
        }
        if after.len() < size {
            out.push_str(after);
            break;
        }
        out.push_str(&after[..size]);
        rest = after[size..].strip_prefix("\r\n").unwrap_or("");
    }
    out
}

// ---------------------------------------------------------------------------
// Phases
// ---------------------------------------------------------------------------

async fn sync_phase(c: &Client) -> anyhow::Result<bool> {
    println!("## sync — deployed webhook answers within the request");
    let mut ok = true;

    let (status, body) = c
        .request("POST", "/receipts", Some(in_spec_receipt("p-1001")))
        .await?;
    check(&mut ok, "in-spec: 200", status == 200);
    check(
        &mut ok,
        "in-spec: receipt_id + empty holds",
        body["receipt_id"].is_string() && as_array(&body["holds"]).is_empty(),
    );

    let (status, body) = c
        .request(
            "POST",
            "/receipts",
            Some(receipt(
                "p-1002",
                "acme",
                "hq",
                &[("resin-a", "60.000", "13.20", "60.010")],
            )),
        )
        .await?;
    let holds = as_array(&body["holds"]);
    check(
        &mut ok,
        "out-of-spec: 200 with one open hold",
        status == 200 && holds.len() == 1 && holds[0]["status"] == json!("open"),
    );
    check(
        &mut ok,
        "hold reason names the moisture exceedance",
        holds
            .first()
            .and_then(|h| h["reason"].as_str())
            .is_some_and(|r| r.contains("moisture")),
    );

    let (status, body) = c
        .request("POST", "/receipts", Some(json!({ "receipt_no": "p-bad" })))
        .await?;
    check(
        &mut ok,
        "malformed: 400 invalid-input",
        status == 400 && body["error"]["code"] == json!("invalid-input"),
    );

    let (status, _) = c.request("GET", "/receipts", None).await?;
    check(&mut ok, "GET => 405", status == 405);
    Ok(ok)
}

async fn burst_phase(c: &Client) -> anyhow::Result<bool> {
    println!("## burst — 20 receipts (3 out-of-spec) over the network");
    let mut ok = true;
    let mut clean = 0;
    for (payload, expected_holds) in burst() {
        let no = payload["receipt_no"].as_str().unwrap_or("").to_string();
        let (status, body) = c.request("POST", "/receipts", Some(payload)).await?;
        let holds = as_array(&body["holds"]);
        if status == 200 && holds.len() == expected_holds {
            clean += 1;
        } else {
            check(
                &mut ok,
                &format!(
                    "{no}: 200 with {expected_holds} holds (got {status}, {})",
                    holds.len()
                ),
                false,
            );
        }
    }
    check(&mut ok, "all 20 receipts answered as expected", clean == 20);
    Ok(ok)
}

async fn audit_phase(args: &F1ProofArgs) -> anyhow::Result<bool> {
    println!("## audit — write-ahead runs + node_runs trace + holds in the DB");
    let mut ok = true;
    let admin_url = args
        .admin_database_url
        .clone()
        .context("no admin database url: pass --admin-database-url or set WAMN_PG_ADMIN_URL")?;
    if !args
        .schema
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_')
    {
        bail!("invalid schema {:?}", args.schema);
    }

    let (db, conn) = tokio_postgres::connect(&admin_url, NoTls).await?;
    let conn_task = tokio::spawn(conn);
    let result = async {
        db.batch_execute(&format!("SET search_path TO {};", args.schema))
            .await?;
        // Every burst receipt has its completed write-ahead run, payload intact.
        let runs: i64 = db
            .query_one(
                "SELECT count(*) FROM runs WHERE input_json->>'receipt_no' LIKE 'r-10%' \
                 AND status = 'completed' AND trigger_source = 'webhook'",
                &[],
            )
            .await?
            .get(0);
        check(&mut ok, "20 completed write-ahead runs", runs == 20);
        // The burst's holds landed (>=: the sync phase adds its own).
        let holds: i64 = db
            .query_one(
                "SELECT count(*) FROM quality_holds WHERE status = 'open'",
                &[],
            )
            .await?
            .get(0);
        check(
            &mut ok,
            &format!("at least {BURST_HOLDS} open holds"),
            holds >= BURST_HOLDS as i64,
        );
        // Traceability: the moisture-exceedance run branched through holds.
        let trace: i64 = db
            .query_one(
                "SELECT count(*) FROM node_runs n JOIN runs r ON n.run_id = r.run_id \
                 WHERE r.input_json->>'receipt_no' = 'r-1005' \
                 AND n.node_id = 'evaluate' AND n.output_port = 'out-of-spec'",
                &[],
            )
            .await?
            .get(0);
        check(
            &mut ok,
            "r-1005 evaluate recorded on out-of-spec",
            trace == 1,
        );
        anyhow::Ok(())
    }
    .await;
    drop(db);
    let _ = conn_task.await;
    result?;
    Ok(ok)
}

async fn rest_phase(rest: &Client) -> anyhow::Result<bool> {
    println!("## rest — deployed generated-REST gateway lists the holds");
    let mut ok = true;
    let (status, body) = rest
        .request("GET", "/api/rest/quality_holds?limit=100", None)
        .await?;
    check(&mut ok, "GET quality-holds 200", status == 200);
    let rows = as_array(&body);
    check(
        &mut ok,
        &format!("at least {BURST_HOLDS} holds visible via REST"),
        rows.len() >= BURST_HOLDS,
    );
    let (status, body) = rest
        .request("GET", "/api/rest/quality_holds?expand=line&limit=100", None)
        .await?;
    check(
        &mut ok,
        "expand=line embeds the receipt line",
        status == 200
            && as_array(&body)
                .iter()
                .all(|r| r["line"]["quantity"].is_string()),
    );
    Ok(ok)
}

pub async fn run(args: F1ProofArgs) -> anyhow::Result<()> {
    println!("# wamn-host f1proof — deployed POC-F1 path over real HTTP");
    let webhook = Client::new(&args.url, &args.host);
    let rest = Client::new(&args.rest_url, &args.rest_host);

    let mut pass = true;
    let want = |m: Mode| args.mode == m || args.mode == Mode::All;
    if want(Mode::Sync) {
        pass &= sync_phase(&webhook).await?;
    }
    if want(Mode::Burst) {
        pass &= burst_phase(&webhook).await?;
        pass &= audit_phase(&args).await?;
    }
    if want(Mode::Rest) {
        pass &= rest_phase(&rest).await?;
    }

    println!("\nf1proof complete — overall PASS: {pass}");
    if !pass {
        bail!("f1proof failed");
    }
    Ok(())
}
