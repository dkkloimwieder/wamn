pub(crate) fn escape_identifier(input: &str) -> String {
    format!("\"{}\"", input.replace('"', "\"\""))
}

pub(crate) fn escape_literal(input: &str) -> String {
    let quoted = input.replace('\'', "''");
    if quoted.contains('\\') {
        format!(" E'{}'", quoted.replace('\\', "\\\\"))
    } else {
        format!("'{quoted}'")
    }
}
