//! Repository-level conformance for the caller-facing flow invocation ABI.

#[cfg(test)]
use wamn_flow_invocation::{
    Admitted, BeginResult, Failure, FlowError, InvokeResult, Rejection, Response,
};

#[cfg(test)]
const WIT: &str = include_str!("../../../crates/execution/flow-invocation/wit/package.wit");
#[cfg(test)]
const FLOWRUNNER_WIT: &str = include_str!("../../../components/execution/flowrunner/wit/world.wit");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flow_invocation_is_versioned_and_two_stage() {
        let code = WIT
            .lines()
            .filter(|line| !line.trim_start().starts_with("///"))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(WIT.contains("package wamn:flow-invocation@0.1.0;"));
        assert!(WIT.contains("begin: func(req: invoke-request) -> begin-result;"));
        assert!(
            WIT.contains("wait: func(run-id: string, timeout-ms: u32) -> option<invoke-result>;")
        );
        assert!(WIT.contains("cancel: func(run-id: string) -> cancel-ack;"));
        assert!(!code.contains("wamn:node"));
    }

    #[test]
    fn flowrunner_exact_claimed_export_is_fenced_and_distinct_from_run_next() {
        assert!(FLOWRUNNER_WIT.contains(
            "export execute-claimed: func(\n    run-id: string,\n    lease-owner: string,\n    lease-generation: s64,\n    lease-ttl-ms: u64,\n  ) -> result<u32, string>;"
        ));
        assert!(FLOWRUNNER_WIT.contains(
            "export run-next: func(lease-ttl-ms: u64) -> result<tuple<bool, option<string>, u32>, string>;"
        ));
        let exact = FLOWRUNNER_WIT
            .split("export execute-claimed:")
            .nth(1)
            .expect("execute-claimed export");
        assert!(!exact.lines().take(7).any(|line| line.contains("option<")));
    }

    #[test]
    fn admitted_and_rejected_results_cannot_share_run_identity() {
        let admitted = BeginResult::Admitted(Admitted {
            run_id: "run-1".to_string(),
        });
        let rejected = BeginResult::Rejected(Rejection {
            status: 409,
            code: "idempotency-scope-changed".to_string(),
        });

        assert!(matches!(admitted, BeginResult::Admitted(_)));
        assert!(matches!(rejected, BeginResult::Rejected(_)));
        assert!(
            !WIT.split("record rejection {")
                .nth(1)
                .expect("rejection record")
                .split('}')
                .next()
                .expect("rejection fields")
                .contains("run-id")
        );
    }

    #[test]
    fn stored_outcomes_keep_status_and_run_identity() {
        let response = InvokeResult::Responded(Response {
            run_id: "run-response".to_string(),
            body: "{}".to_string(),
            status_hint: Some(202),
        });
        let failed = InvokeResult::Failed(Failure {
            status: 400,
            error: FlowError {
                code: "authored-fail".to_string(),
                message: None,
                run_id: "run-failed".to_string(),
                flow_id: "flow-1".to_string(),
                flow_version: 3,
            },
        });

        assert!(matches!(response, InvokeResult::Responded(_)));
        assert!(matches!(
            failed,
            InvokeResult::Failed(Failure { status: 400, .. })
        ));
    }
}
