//! Shared kind retention policy.
pub fn keep_on_failure(value: Option<&str>) -> anyhow::Result<bool> {
    match value {
        None | Some("") => Ok(true),
        Some("never") => Ok(false),
        Some(value) => {
            anyhow::bail!("invalid WAMN_KIND_KEEP={value:?}: accepted value is never (or unset)")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::keep_on_failure;

    #[test]
    fn keep_defaults_to_failure_and_refuses_other_values() -> anyhow::Result<()> {
        assert!(keep_on_failure(None)?);
        assert!(keep_on_failure(Some(""))?);
        assert!(!keep_on_failure(Some("never"))?);
        for value in ["always", "Never", " "] {
            assert!(
                keep_on_failure(Some(value))
                    .unwrap_err()
                    .to_string()
                    .contains("WAMN_KIND_KEEP")
            );
        }
        Ok(())
    }
}
