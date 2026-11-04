use super::BaselineMismatch;

pub(super) fn compare_digest(
    installed: Option<&str>,
    candidate: &str,
) -> Result<(), BaselineMismatch> {
    if installed == Some(candidate) {
        Ok(())
    } else {
        Err(BaselineMismatch {
            installed: installed.map(str::to_owned),
            candidate: candidate.to_owned(),
        })
    }
}
