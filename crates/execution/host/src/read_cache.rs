//! The ETag of a read route and the If-None-Match comparison.
//!
//! A `get` has a strong ETag from the release and the revision of the record
//! it returns. A `query` or `projection` has a weak ETag from the release and
//! the model versions of the relations it reads (`deploy/sql/model-versions.sql`).
//! A new release changes every tag, because it can change the response bytes.
//! The host compares If-None-Match with the tag and answers not-modified on a
//! match (`docs/plan/http-reads.md` section 4.3).

use serde_json::{Value, json};
use wamn_catalog::ServingRelation;

/// The strong ETag of a `get` result, or `None` when the result carries no
/// revision.
///
/// A read carries one item, so its result is a list of one outcome. Only a
/// completed outcome whose value holds the revision field has a tag. A refusal
/// or a missing record has none.
pub(crate) fn get_tag(release: &str, result: &Value, revision_field: &str) -> Option<String> {
    let [outcome] = result.as_array()?.as_slice() else {
        return None;
    };
    let revision = outcome.get("value")?.get(revision_field)?;
    if !(revision.is_number() || revision.is_string()) {
        return None;
    }
    Some(format!(
        "\"{}\"",
        digest(&json!(["get", release, revision]))
    ))
}

/// The weak ETag of a list over `reads`, each paired with its model version.
pub(crate) fn list_tag(release: &str, versions: &[(&ServingRelation, i64)]) -> String {
    let versions = versions
        .iter()
        .map(|(relation, version)| json!([relation.schema, relation.relation, version]))
        .collect::<Vec<_>>();
    format!("W/\"{}\"", digest(&json!(["list", release, versions])))
}

/// Whether an If-None-Match value matches `tag`.
///
/// If-None-Match uses the weak comparison: two tags match when their opaque
/// parts are equal, whether or not either is weak. `*` matches any current
/// representation.
pub(crate) fn matches(if_none_match: &str, tag: &str) -> bool {
    let opaque = |candidate: &str| candidate.trim().trim_start_matches("W/").to_owned();
    let tag = opaque(tag);
    if_none_match
        .split(',')
        .any(|candidate| candidate.trim() == "*" || opaque(candidate) == tag)
}

/// The first 32 hex digits of the SHA-256 of the canonical JSON of `value`.
fn digest(value: &Value) -> String {
    let hash = wamn_execution_contract::canonical_json_sha256(value);
    let hex = &hash["sha256:".len()..];
    hex[..32].to_owned()
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use wamn_catalog::ServingRelation;

    use super::{get_tag, list_tag, matches};

    fn relation(name: &str) -> ServingRelation {
        ServingRelation {
            schema: "wms".to_owned(),
            relation: name.to_owned(),
        }
    }

    #[test]
    fn a_get_tag_is_strong_and_follows_the_release_and_the_revision() {
        let result = json!([{"value": {"id": "p-1", "row_version": 3}}]);
        let tag = get_tag("sha256:a", &result, "row_version").expect("a completed get has a tag");
        assert!(
            tag.starts_with('"') && tag.ends_with('"') && tag.len() == 34,
            "{tag}"
        );
        assert_eq!(
            get_tag("sha256:a", &result, "row_version"),
            Some(tag.clone())
        );
        let newer = json!([{"value": {"id": "p-1", "row_version": 4}}]);
        assert_ne!(
            get_tag("sha256:a", &newer, "row_version"),
            Some(tag.clone())
        );
        assert_ne!(get_tag("sha256:b", &result, "row_version"), Some(tag));
    }

    #[test]
    fn a_get_without_a_revision_has_no_tag() {
        for result in [
            json!([{"error": {"code": "not_found"}}]),
            json!([{"value": {"id": "p-1"}}]),
            json!([{"value": null}]),
            json!([]),
            json!([{"value": {"row_version": 1}}, {"value": {"row_version": 1}}]),
            json!({"value": {"row_version": 1}}),
        ] {
            assert_eq!(
                get_tag("sha256:a", &result, "row_version"),
                None,
                "{result}"
            );
        }
    }

    #[test]
    fn a_list_tag_is_weak_and_follows_the_release_and_every_version() {
        let (pallet, quantity) = (relation("pallet"), relation("pallet_quantity"));
        let tag = list_tag("sha256:a", &[(&pallet, 2), (&quantity, 5)]);
        assert!(tag.starts_with("W/\"") && tag.ends_with('"'), "{tag}");
        assert_eq!(list_tag("sha256:a", &[(&pallet, 2), (&quantity, 5)]), tag);
        assert_ne!(list_tag("sha256:a", &[(&pallet, 3), (&quantity, 5)]), tag);
        assert_ne!(list_tag("sha256:a", &[(&pallet, 2), (&quantity, 6)]), tag);
        assert_ne!(list_tag("sha256:b", &[(&pallet, 2), (&quantity, 5)]), tag);
    }

    #[test]
    fn if_none_match_uses_the_weak_comparison() {
        assert!(matches("W/\"abc\"", "W/\"abc\""));
        assert!(matches("\"abc\"", "W/\"abc\""));
        assert!(matches("W/\"abc\"", "\"abc\""));
        assert!(matches("\"x\", W/\"abc\"", "W/\"abc\""));
        assert!(matches("*", "\"abc\""));
        assert!(!matches("\"abd\"", "\"abc\""));
        assert!(!matches("", "\"abc\""));
    }
}
