pub(super) fn broker_token(value: &str) -> bool {
    !value.is_empty()
        && !value.chars().any(|character| {
            character.is_whitespace() || character.is_control() || ".*>".contains(character)
        })
}
