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
        assert!(!WIT.contains("cancel:"));
        assert!(!WIT.contains("cancelled(failure)"));
        assert!(!code.contains("outcome-expired"));
        assert!(!code.contains("accepted("));
        assert!(!code.contains("pending("));
        assert!(!code.contains("wamn:node"));
    }

    #[test]
    fn flowrunner_world_is_versioned_and_exports_only_run() {
        let code = FLOWRUNNER_WIT
            .lines()
            .filter(|line| !line.trim_start().starts_with("///"))
            .collect::<Vec<_>>()
            .join("\n");

        assert!(FLOWRUNNER_WIT.contains("package wamn:flowrunner@0.1.0;"));
        assert!(
            code.contains(
                "export run: func(run-id: string, payload: string) -> result<u32, string>;"
            )
        );
        assert_eq!(code.matches("export ").count(), 1);
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
