//! Saved edits retain deployed data and refuse invalid source through product commands.
use anyhow::{Context as _, ensure};
use serde_json::{Value, json};
use sqlx::Row as _;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;
use wamn_postgres::connect;
use wamn_test_application::Application;

const CODE: &str = "apps/wamn_receiving/component/src/reads.rs";
const SQL: &str = "apps/wamn_receiving/query/location.sql";
const MIGRATIONS: &str = "apps/wamn_receiving/migrations";
const CODE_BEFORE: &str = "read::location_list(connection)";
const CODE_AFTER: &str = "async { let mut rows = read::location_list(connection).await?; for row in &mut rows { row.location_code.push_str(\"-edited\"); } Ok::<_, crate::data::AccessError>(rows) }";

#[tokio::test]
async fn local_watch_preserves_data_and_refuses_bad_sql() -> anyhow::Result<()> {
    let application: Application =
        panic!("wamn-eulfr.12.9: no product location and order-line import");
    let repository = super::super::repository_root()?;
    let source = repository.join("apps/client_acme_receiving");
    let source = source.as_path();
    let project_url = application.database_url().await?;
    let mut original = SavedSource::capture(&repository)?;
    let first = Served::deployed(&application).await?;
    let result = async {
        let token = &application.routes.bearer;
        let denied = http_client()?.get(format!("{}/location/list", first.url))
            .header("Host", &first.host).send().await?;
        ensure!(denied.status() == reqwest::StatusCode::UNAUTHORIZED, "the local route must require authentication");
        first.locations(token, &["DOCK-1", "DOCK-2"]).await?;
        let updated = first.request(token, "/purchase_order/update", json!([{
            "request_id":"local-update", "id":"00000000-0000-0000-0000-000000000301",
            "expected_row_version":1, "change":{"supplier_id":"00000000-0000-0000-0000-000000000402"}
        }])).await?;
        ensure!(updated[0]["value"]["row_version"] == 2, "the authenticated command did not commit its revision");

        original.replace(CODE, CODE_BEFORE, CODE_AFTER)?;
        application.rebuild(source).await?;
        let code = Served::deployed(&application).await?;
        code.retained(&first)?;
        code.locations(token, &["DOCK-1-edited", "DOCK-2-edited"]).await?;
        require_revision(&project_url).await?;

        original.replace(SQL, "location.location_code ASC", "location.location_code DESC")?;
        application.rebuild(source).await?;
        let sql = Served::deployed(&application).await?;
        sql.retained(&code)?;
        sql.locations(token, &["DOCK-2-edited", "DOCK-1-edited"]).await?;
        require_revision(&project_url).await?;

        let valid_sql = fs::read(repository.join(SQL))?;
        fs::write(repository.join(SQL), b"SELECT invalid_delivery_sql FROM receiving.location;\n")?;
        ensure!(application.rebuild(source).await.is_err(), "invalid source was accepted");
        sql.locations(token, &["DOCK-2-edited", "DOCK-1-edited"]).await?;
        require_revision(&project_url).await?;
        fs::write(repository.join(SQL), valid_sql)?;
        application.rebuild(source).await?;
        let repaired = Served::deployed(&application).await?;
        repaired.retained(&sql)?;
        repaired.locations(token, &["DOCK-2-edited", "DOCK-1-edited"]).await?;

        let valid_code = fs::read(repository.join(CODE))?;
        let mut invalid_code = valid_code.clone();
        invalid_code.extend_from_slice(b"\ncompile_error!(\"invalid candidate Rust\");\n");
        fs::write(repository.join(CODE), invalid_code)?;
        ensure!(application.rebuild(source).await.is_err(), "invalid source was accepted");
        repaired.locations(token, &["DOCK-2-edited", "DOCK-1-edited"]).await?;
        require_revision(&project_url).await?;
        fs::write(repository.join(CODE), valid_code)?;
        application.rebuild(source).await?;
        let compiled = Served::deployed(&application).await?;
        compiled.retained(&repaired)?;
        compiled.locations(token, &["DOCK-2-edited", "DOCK-1-edited"]).await?;

        Ok::<_, anyhow::Error>(())
    }.await;
    let restored = original.restore();
    application.remove().await?;
    result?;
    restored
}

async fn require_revision(url: &str) -> anyhow::Result<()> {
    let mut client = connect(url).await?;
    let version: i32 = sqlx::query("SELECT row_version FROM receiving.purchase_order WHERE id='00000000-0000-0000-0000-000000000301'").fetch_one(&mut client).await?.get(0);
    ensure!(
        version == 2,
        "a compatible save lost or replayed the authenticated mutation"
    );
    Ok(())
}

fn http_client() -> anyhow::Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(30))
        .build()?)
}

pub(super) struct Served {
    pub(super) url: String,
    pub(super) host: String,
    pub(super) instance: String,
}

impl Served {
    async fn deployed(application: &Application) -> anyhow::Result<Self> {
        Ok(Self {
            url: application.routes.endpoint.clone(),
            host: application.routes.host.clone(),
            instance: application.database_url().await?,
        })
    }

    fn retained(&self, previous: &Self) -> anyhow::Result<()> {
        ensure!(
            self.instance == previous.instance,
            "a compatible edit recreated the application database"
        );
        Ok(())
    }

    async fn request(&self, token: &str, path: &str, body: Value) -> anyhow::Result<Value> {
        Ok(http_client()?
            .post(format!("{}{path}", self.url))
            .header("Host", &self.host)
            .bearer_auth(token)
            .json(&body)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?)
    }

    /// A read whose one request element is empty: a GET with no query and no body.
    async fn get(&self, token: &str, path: &str) -> anyhow::Result<Value> {
        Ok(http_client()?
            .get(format!("{}{path}", self.url))
            .header("Host", &self.host)
            .bearer_auth(token)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?)
    }

    async fn locations(&self, token: &str, expected: &[&str]) -> anyhow::Result<()> {
        let body = self.get(token, "/location/list").await?;
        ensure!(
            body[0].get("request_id").is_none(),
            "the location read reply echoed a request_id"
        );
        let rows = body[0]["value"]["rows"]
            .as_array()
            .context("the authenticated location result has rows")?;
        let codes = rows
            .iter()
            .map(|row| row["location_code"].as_str())
            .collect::<Vec<_>>();
        ensure!(
            codes == expected.iter().copied().map(Some).collect::<Vec<_>>(),
            "the served SQL or retained rows differ: {body}"
        );
        Ok(())
    }
}

struct SavedSource {
    repository: PathBuf,
    files: BTreeMap<PathBuf, (Vec<u8>, fs::Permissions)>,
    directories: BTreeMap<PathBuf, fs::Permissions>,
    outputs: Vec<PathBuf>,
    restored: bool,
}

impl SavedSource {
    fn capture(repository: &Path) -> anyhow::Result<Self> {
        let mut files = BTreeMap::new();
        let mut directories = BTreeMap::new();
        for relative in [CODE, SQL] {
            capture_file(repository, &repository.join(relative), &mut files)?;
        }
        let mut outputs = Vec::new();
        // The appended migration is a new file, so restore owns the whole directory.
        capture_tree(
            repository,
            &repository.join(MIGRATIONS),
            &mut files,
            &mut directories,
        )?;
        outputs.push(PathBuf::from(MIGRATIONS));
        for package in ["wamn_receiving", "client_acme_receiving"] {
            let package = PathBuf::from("apps").join(package);

            for relative in [wamn_compiler::output_root(&package)] {
                capture_tree(
                    repository,
                    &repository.join(&relative),
                    &mut files,
                    &mut directories,
                )?;
                outputs.push(relative);
            }
        }
        Ok(Self {
            repository: repository.to_owned(),
            files,
            directories,
            outputs,
            restored: false,
        })
    }

    fn replace(&self, relative: &str, before: &str, after: &str) -> anyhow::Result<()> {
        let path = self.repository.join(relative);
        let bytes = fs::read_to_string(&path)?;
        ensure!(
            bytes.matches(before).count() == 1,
            "the existing edit anchor moved: {relative}"
        );
        fs::write(path, bytes.replace(before, after))?;
        Ok(())
    }

    fn restore(&mut self) -> anyhow::Result<()> {
        if self.restored {
            return Ok(());
        }
        for relative in &self.outputs {
            let path = self.repository.join(relative);
            if path.exists() {
                fs::remove_dir_all(path)?;
            }
        }
        for relative in self.directories.keys() {
            fs::create_dir_all(self.repository.join(relative))?;
        }
        for (relative, (bytes, permissions)) in &self.files {
            let path = self.repository.join(relative);
            fs::create_dir_all(path.parent().context("the owned file has a parent")?)?;
            fs::write(&path, bytes)?;
            fs::set_permissions(path, permissions.clone())?;
        }
        for (relative, permissions) in &self.directories {
            fs::set_permissions(self.repository.join(relative), permissions.clone())?;
        }
        self.restored = true;
        Ok(())
    }
}

impl Drop for SavedSource {
    fn drop(&mut self) {
        if let Err(error) = self.restore() {
            eprintln!("restore the owned local test source: {error:#}");
        }
    }
}

fn capture_file(
    repository: &Path,
    path: &Path,
    files: &mut BTreeMap<PathBuf, (Vec<u8>, fs::Permissions)>,
) -> anyhow::Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    ensure!(
        metadata.is_file(),
        "the owned source backup requires ordinary files"
    );
    files.insert(
        path.strip_prefix(repository)?.to_owned(),
        (fs::read(path)?, metadata.permissions()),
    );
    Ok(())
}

fn capture_tree(
    repository: &Path,
    directory: &Path,
    files: &mut BTreeMap<PathBuf, (Vec<u8>, fs::Permissions)>,
    directories: &mut BTreeMap<PathBuf, fs::Permissions>,
) -> anyhow::Result<()> {
    if !directory.exists() {
        return Ok(());
    }
    ensure!(
        fs::symlink_metadata(directory)?.is_dir(),
        "generated outputs must be owned directories"
    );
    directories.insert(
        directory.strip_prefix(repository)?.to_owned(),
        fs::metadata(directory)?.permissions(),
    );
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            capture_tree(repository, &entry.path(), files, directories)?;
        } else {
            capture_file(repository, &entry.path(), files)?;
        }
    }
    Ok(())
}
