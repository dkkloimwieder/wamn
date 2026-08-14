//! Black-box guards for the retained run-worker component surface.

#[cfg(test)]
mod tests {
    const FLOWRUNNER_WIT: &str =
        include_str!("../../../components/execution/flowrunner/wit/world.wit");

    #[test]
    fn flowrunner_is_versioned_at_zero_one_and_exports_run_alone() {
        let code = FLOWRUNNER_WIT
            .lines()
            .filter(|line| !line.trim_start().starts_with("///"))
            .collect::<Vec<_>>()
            .join("\n");

        assert!(code.contains("package wamn:flowrunner@0.1.0;"));
        assert!(
            code.contains(
                "export run: func(run-id: string, payload: string) -> result<u32, string>;"
            )
        );
        assert_eq!(code.matches("export ").count(), 1);
    }
}
