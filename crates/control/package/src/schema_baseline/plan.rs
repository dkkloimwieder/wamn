use super::BaselineDifference;

pub(super) fn compare_digest(
    installed: Option<&str>,
    candidate: &str,
) -> Option<BaselineDifference> {
    (installed != Some(candidate)).then(|| BaselineDifference {
        installed: installed.map(str::to_owned),
        candidate: candidate.to_owned(),
    })
}
