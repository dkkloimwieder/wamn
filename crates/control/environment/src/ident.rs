//! Small identifier tests shared by control-plane effect shells.

pub fn is_bare_ident(value: &str) -> bool {
    wamn_postgres::schema::BareSchemaName::new(value).is_ok()
}
