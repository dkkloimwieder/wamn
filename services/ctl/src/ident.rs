//! Small identifier checks shared by control-plane effect shells.

pub(crate) fn is_bare_ident(value: &str) -> bool {
    wamn_schema_control::BareSchemaName::new(value).is_ok()
}
