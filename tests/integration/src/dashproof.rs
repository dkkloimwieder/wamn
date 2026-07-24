//! `dashproof` ([9.9], wamn-b4e): the DEPLOYED proof that the 9.9 dashboards
//! layer stands up. There is no emission seam to drive (unlike `metricbench` →
//! `:8889`), so this follows the `traceproof`/`apiproof` shape — assert against a
//! running Grafana's HTTP API, not scaffolding. It proves, each a NAMED failure:
//!
//!   1. `GET /api/health` -> `database: ok` (Grafana + its DB are up);
//!   2. `GET /api/datasources` -> Prometheus + Tempo + Loki all present (by the
//!      FIXED uids deploy/infra/grafana.yaml provisions), and each datasource's
//!      `GET /api/datasources/uid/<uid>/health` is OK. Honest-skip policy
//!      (metricbench phase-6 precedent): Prometheus health is HARD (cheap to
//!      stand up locally); Tempo/Loki health is soft in `--local` (their
//!      containers may be absent), HARD in-cluster where they exist;
//!   3. `GET /api/search?type=dash-db` + `GET /api/folders` -> the STATIC SRE
//!      dashboard + its folder are file-provisioned and present;
//!   4. after `provision-dashboards` has run: for EVERY registry org (read from
//!      `--system-database-url`, plus any `--expect-tenant`), its per-tenant
//!      folder + dashboard are present.
//!
//! Auth: admin Basic-auth from the grafana-admin Secret
//! (GF_SECURITY_ADMIN_USER / GF_SECURITY_ADMIN_PASSWORD) — `/api/datasources`
//! needs an authenticated admin/editor. The SRE identity + the per-tenant
//! folder/dashboard uids are the SAME `wamn_ctl::provision_dashboards` values the
//! verb writes (one source, no cross-crate drift). In-cluster gate of record:
//! `deploy/gates/dashproof-job.yaml`.

use anyhow::{Context as _, bail};
use clap::Args;
use serde_json::Value;
use tokio_postgres::NoTls;

use wamn_ctl::provision_dashboards::{
    DS_LOKI, DS_PROM, DS_TEMPO, SRE_DASHBOARD_TITLE, SRE_DASHBOARD_UID, SRE_FOLDER_TITLE,
    basic_auth, http_json, tenant_dashboard_uid, tenant_folder_title, tenant_folder_uid,
};

#[derive(Debug, Args)]
pub struct DashproofArgs {
    /// Grafana base URL, e.g. http://grafana:3000.
    #[arg(long)]
    pub grafana_url: String,

    /// Grafana admin user (Basic auth). Env GF_SECURITY_ADMIN_USER.
    #[arg(long, env = "GF_SECURITY_ADMIN_USER", default_value = "admin")]
    pub user: String,

    /// Grafana admin password (Basic auth). Env GF_SECURITY_ADMIN_PASSWORD.
    #[arg(long, env = "GF_SECURITY_ADMIN_PASSWORD", default_value = "admin")]
    pub password: String,

    /// Soft-skip Tempo/Loki datasource health (their backends may be absent in a
    /// local docker run). Default OFF — in-cluster all three are HARD.
    #[arg(long)]
    pub local: bool,

    /// Expect this org's per-tenant folder + dashboard (repeatable). Additive to
    /// the orgs read from --system-database-url.
    #[arg(long = "expect-tenant")]
    pub expect_tenant: Vec<String>,

    /// Superuser Postgres URL to the T1 system DB — enumerate `registry.orgs` so
    /// EVERY provisioned org's folder is asserted. Env WAMN_SYSTEM_ADMIN_URL.
    #[arg(long, env = "WAMN_SYSTEM_ADMIN_URL")]
    pub system_database_url: Option<String>,
}

pub async fn run(args: DashproofArgs) -> anyhow::Result<()> {
    let base = args.grafana_url.trim_end_matches('/');
    let auth = basic_auth(&args.user, &args.password);
    println!(
        "# wamn-gates [9.9] dashproof -> {base} (local={})",
        args.local
    );

    let mut pass = true;

    // === (1) /api/health -> database: ok (unauth) =========================
    let (h_status, h_body) = http_json(base, "GET", "/api/health", None, None)
        .await
        .context("GET /api/health")?;
    let db_ok = h_status == 200
        && serde_json::from_str::<Value>(&h_body)
            .ok()
            .and_then(|v| {
                v.get("database")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .as_deref()
            == Some("ok");
    check(&mut pass, "(1) /api/health database ok", db_ok, &h_body);

    // === (2) datasources present + healthy ================================
    let (ds_status, ds_body) = http_json(base, "GET", "/api/datasources", Some(&auth), None)
        .await
        .context("GET /api/datasources")?;
    let datasources: Vec<Value> = if ds_status == 200 {
        serde_json::from_str(&ds_body).unwrap_or_default()
    } else {
        Vec::new()
    };
    let uid_present = |uid: &str| {
        datasources
            .iter()
            .any(|d| d.get("uid").and_then(Value::as_str) == Some(uid))
    };
    for (label, uid) in [
        ("prometheus", DS_PROM),
        ("tempo", DS_TEMPO),
        ("loki", DS_LOKI),
    ] {
        check(
            &mut pass,
            &format!("(2a) datasource {label} present (uid {uid})"),
            uid_present(uid),
            &format!("status {ds_status}, {} datasource(s)", datasources.len()),
        );
    }

    // Datasource reachability: Prometheus HARD; Tempo/Loki soft in --local.
    for (label, uid, hard) in [
        ("prometheus", DS_PROM, true),
        ("tempo", DS_TEMPO, !args.local),
        ("loki", DS_LOKI, !args.local),
    ] {
        let healthy = datasource_healthy(base, &auth, uid).await;
        if hard {
            check(
                &mut pass,
                &format!("(2b) datasource {label} health OK"),
                healthy,
                "health endpoint did not report OK",
            );
        } else if healthy {
            println!("## (2b) datasource {label} health OK -> PASS");
        } else {
            println!("## (2b) datasource {label} health -> SKIP (--local; backend not stood up)");
        }
    }

    // === (3) static SRE dashboard + folder present ========================
    let (_, search_body) = http_json(base, "GET", "/api/search?type=dash-db", Some(&auth), None)
        .await
        .context("GET /api/search")?;
    let dashboards: Vec<Value> = serde_json::from_str(&search_body).unwrap_or_default();
    let sre_present = dashboards.iter().any(|d| {
        d.get("uid").and_then(Value::as_str) == Some(SRE_DASHBOARD_UID)
            || d.get("title").and_then(Value::as_str) == Some(SRE_DASHBOARD_TITLE)
    });
    check(
        &mut pass,
        "(3a) SRE dashboard present",
        sre_present,
        &format!(
            "{} dashboard(s); want uid {SRE_DASHBOARD_UID:?}",
            dashboards.len()
        ),
    );

    let folders = fetch_folders(base, &auth).await?;
    let sre_folder = folders
        .iter()
        .any(|f| f.get("title").and_then(Value::as_str) == Some(SRE_FOLDER_TITLE));
    check(
        &mut pass,
        "(3b) SRE folder present",
        sre_folder,
        &format!(
            "{} folder(s); want title {SRE_FOLDER_TITLE:?}",
            folders.len()
        ),
    );

    // === (4) per-tenant folders + dashboards for every registry org =======
    let mut orgs = args.expect_tenant.clone();
    if let Some(sys) = &args.system_database_url {
        orgs.extend(read_orgs(sys).await.context("read registry.orgs")?);
    }
    orgs.sort();
    orgs.dedup();

    if orgs.is_empty() {
        let why = if args.system_database_url.is_some() {
            "registry.orgs has 0 rows and no --expect-tenant given"
        } else {
            "no --expect-tenant / --system-database-url given"
        };
        println!("## (4) per-tenant folders -> SKIP ({why})");
    } else {
        let folder_uids: std::collections::HashSet<&str> = folders
            .iter()
            .filter_map(|f| f.get("uid").and_then(Value::as_str))
            .collect();
        let folder_titles: std::collections::HashSet<&str> = folders
            .iter()
            .filter_map(|f| f.get("title").and_then(Value::as_str))
            .collect();
        let dash_uids: std::collections::HashSet<&str> = dashboards
            .iter()
            .filter_map(|d| d.get("uid").and_then(Value::as_str))
            .collect();
        for org in &orgs {
            let fu = tenant_folder_uid(org);
            let du = tenant_dashboard_uid(org);
            let ft = tenant_folder_title(org);
            let ok = (folder_uids.contains(fu.as_str()) || folder_titles.contains(ft.as_str()))
                && dash_uids.contains(du.as_str());
            check(
                &mut pass,
                &format!("(4) tenant {org:?} folder + dashboard present"),
                ok,
                &format!("want folder {fu:?} + dashboard {du:?}"),
            );
        }
    }

    println!("\ndashproof complete — overall PASS: {pass}");
    if !pass {
        bail!("dashproof gate failed");
    }
    Ok(())
}

/// `GET /api/datasources/uid/<uid>/health` -> `status: OK` (the datasource can
/// reach its backend). Any transport error or non-OK reads as unhealthy — except
/// a 404: a frontend-only plugin (Tempo in Grafana 11) registers no backend
/// health resource on this route, so fall back to the datasource PROXY echo
/// (`/api/datasources/proxy/uid/<uid>/api/echo`), which proves the same
/// property: Grafana can reach the backend and the backend answers.
async fn datasource_healthy(base: &str, auth: &str, uid: &str) -> bool {
    let path = format!("/api/datasources/uid/{uid}/health");
    match http_json(base, "GET", &path, Some(auth), None).await {
        Ok((200, body)) => {
            serde_json::from_str::<Value>(&body)
                .ok()
                .and_then(|v| {
                    v.get("status")
                        .and_then(Value::as_str)
                        .map(str::to_ascii_uppercase)
                })
                .as_deref()
                == Some("OK")
        }
        Ok((404, _)) => {
            let proxy = format!("/api/datasources/proxy/uid/{uid}/api/echo");
            matches!(
                http_json(base, "GET", &proxy, Some(auth), None).await,
                Ok((200, _))
            )
        }
        _ => false,
    }
}

async fn fetch_folders(base: &str, auth: &str) -> anyhow::Result<Vec<Value>> {
    let (_, body) = http_json(base, "GET", "/api/folders?limit=1000", Some(auth), None)
        .await
        .context("GET /api/folders")?;
    Ok(serde_json::from_str(&body).unwrap_or_default())
}

/// The org ids recorded in the T1 registry (the same read `provision-dashboards`
/// enumerates), so dashproof checks a folder for EVERY provisioned org.
async fn read_orgs(system_url: &str) -> anyhow::Result<Vec<String>> {
    let (client, conn) = tokio_postgres::connect(system_url, NoTls)
        .await
        .context("system db connect")?;
    let conn_task = tokio::spawn(conn);
    let result = async {
        let rows = client
            .query("SELECT id FROM registry.orgs ORDER BY id", &[])
            .await
            .context("SELECT registry.orgs")?;
        anyhow::Ok(rows.iter().map(|r| r.get::<_, String>(0)).collect())
    }
    .await;
    drop(client);
    let _ = conn_task.await;
    result
}

fn check(pass: &mut bool, label: &str, ok: bool, detail: &str) {
    if ok {
        println!("## {label} -> PASS");
    } else {
        *pass = false;
        println!("## {label} -> FAIL ({detail})");
    }
}
