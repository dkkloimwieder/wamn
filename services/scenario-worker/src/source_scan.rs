//! The one way this crate's structural guards read their own source.
//!
//! A guard that `include_str!`s its own file and then locates an implementation
//! span by searching for signature text has a defect: the search literal is
//! spelled in the same file it scans, so a DELETED subject is still "found" —
//! the guard matches its own search string. The `expect` never fires, any span
//! bounded by that position collapses onto the test source, and the guard
//! silently becomes a no-op. Such guards pass only by positional luck, because
//! the real definition happens to precede the test module and `find` reaches it
//! first; any deletion or reordering converts them into no-ops.
//!
//! Every scan here is therefore taken over the implementation half alone, which
//! is the idiom ruled at `tests/conformance/src/runtime_inventory.rs:18`. The
//! boundary literal is immune to the same defect because in source it is
//! written with an escaped `\n` rather than a literal newline, so it cannot
//! match its own spelling. One constant serves the whole crate rather than each
//! module restating it.

/// Where a module's implementation ends and its own tests begin.
const CFG_TEST_MODULE: &str = "#[cfg(test)]\nmod tests {";

/// Return the implementation half of one module's source, never its tests.
///
/// Panics when the boundary is absent: a module whose tests are introduced some
/// other way must fail loudly here rather than silently hand every guard the
/// whole file back.
pub(crate) fn implementation(source: &str) -> &str {
    source
        .split_once(CFG_TEST_MODULE)
        .expect("the module carries its tests behind the canonical boundary")
        .0
}

/// Return the implementation slice of one MODULE SOURCE between two markers.
///
/// Both markers are located in the implementation half only, so a deleted
/// subject panics instead of matching the search literal in the caller. Pass
/// [`within`] an already-narrowed span instead: this one requires the whole
/// module, boundary included.
pub(crate) fn between<'a>(source: &'a str, start: &str, end: &str) -> &'a str {
    within(implementation(source), start, end)
}

/// Return the slice of an ALREADY-NARROWED span between two markers.
///
/// The span must already have come from [`between`] or [`implementation`], so
/// the test half is out of reach before this is called.
pub(crate) fn within<'a>(span: &'a str, start: &str, end: &str) -> &'a str {
    span.split(start)
        .nth(1)
        .unwrap_or_else(|| panic!("{start} exists"))
        .split(end)
        .next()
        .unwrap_or_else(|| panic!("{end} follows {start}"))
}

#[cfg(test)]
mod tests {
    use super::{between, implementation};

    /// A module shaped like this crate's own: an implementation, the canonical
    /// boundary, and a test that names the very text it scans for.
    const MODULE: &str = concat!(
        "fn subject() {}\n\nfn other() {}\n\n",
        "#[cfg(test)]\n",
        "mod tests {\n    let span = between(source, \"fn subject()\", \"fn other()\");\n}\n"
    );

    #[test]
    fn a_scan_never_sees_the_tests_that_name_their_own_subject() {
        let implementation = implementation(MODULE);
        assert!(implementation.contains("fn subject() {}"));
        assert!(
            !implementation.contains("mod tests"),
            "the scan reached the test module: {implementation}"
        );
        assert_eq!(between(MODULE, "fn subject()", "fn other()"), " {}\n\n");
    }

    /// The defect this module exists to prevent: with the subject deleted, the
    /// only remaining match is the test's own search literal, and a whole-file
    /// scan would return that instead of panicking.
    #[test]
    #[should_panic(expected = "fn subject() exists")]
    fn a_deleted_subject_panics_rather_than_matching_the_search_literal() {
        let deleted = MODULE.replace("fn subject() {}\n\n", "");
        // The literal still occurs, inside the test half, so a naive whole-file
        // `split(..).nth(1)` still succeeds here. Scanning the implementation
        // half is what turns that silent pass into this panic.
        assert!(deleted.contains("fn subject()"));
        between(&deleted, "fn subject()", "fn other()");
    }

    #[test]
    #[should_panic(expected = "the module carries its tests behind the canonical boundary")]
    fn a_module_without_the_canonical_boundary_fails_loudly() {
        implementation("fn subject() {}\n");
    }
}
