use super::Path;

pub fn ensure_secret_path(path: &Path, flag: &str) -> anyhow::Result<()> {
    anyhow::ensure!(
        path.as_os_str() != "-",
        "{flag} must name a file; '-' and stdout are forbidden for credentials"
    );
    Ok(())
}
