//! The database service hostname.
use std::borrow::Cow;

pub(crate) fn database_host<'a>(
    explicit: Option<&'a str>,
    cluster: &str,
    namespace: &str,
) -> Cow<'a, str> {
    match explicit {
        Some(host) => Cow::Borrowed(host),
        None => Cow::Owned(format!("{cluster}-rw.{namespace}.svc.cluster.local")),
    }
}
