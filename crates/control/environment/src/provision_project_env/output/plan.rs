use super::{Path, PathBuf};

pub fn ensure_secret_path(path: &Path, flag: &str) -> anyhow::Result<()> {
    anyhow::ensure!(
        path.as_os_str() != "-",
        "{flag} must name a file; '-' and stdout are forbidden for credentials"
    );
    Ok(())
}

pub(super) fn ensure_distinct_secret_path(
    seen: &[(&str, PathBuf)],
    flag: &str,
    comparable: &PathBuf,
) -> anyhow::Result<()> {
    if let Some((other_flag, _)) = seen.iter().find(|(_, other_path)| other_path == comparable) {
        anyhow::bail!("{flag} and {other_flag} must name distinct credential output files");
    }
    Ok(())
}
