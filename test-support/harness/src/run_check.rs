//! Run results retained by failed cluster checks.

use std::path::Path;

use anyhow::Context as _;
use serde_json::{Value, json};

/// Attach the case's run status, failure and output to a failed check.
// Box at the call boundary so nested checks do not enlarge their callers' futures.
pub fn check<T>(
    directory: &Path,
    helper: &str,
    project: &tokio_postgres::Client,
    checking: impl Future<Output = anyhow::Result<T>>,
) -> impl Future<Output = anyhow::Result<T>> {
    Box::pin(check_with_results(directory, helper, checking, async {
        Ok(project
            .query_one(
                "SELECT COALESCE(jsonb_agg(jsonb_build_object(\
                 'run_id', run_id, 'env_key', env_key, 'package_id', package_id, \
                 'wiring_id', wiring_id, 'status', status, 'fail_type', fail_type, \
                 'result_json', result_json) ORDER BY created_at, run_id), '[]'::jsonb) \
                 FROM wamn_run.runs",
                &[],
            )
            .await?
            .get(0))
    }))
}

async fn check_with_results<T>(
    directory: &Path,
    helper: &str,
    checking: impl Future<Output = anyhow::Result<T>>,
    results: impl Future<Output = anyhow::Result<Value>>,
) -> anyhow::Result<T> {
    let error = match checking.await {
        Ok(value) => return Ok(value),
        Err(error) => error,
    };
    let runs = results.await.unwrap_or_else(
        |error| json!({"status":"unavailable", "error":format!("{error:#}"), "result_json":null}),
    );
    let observed = json!({"helper":helper, "error":format!("{error:#}"), "runs":runs});
    let file = directory.join(format!("run-result-{helper}.json"));
    let kept = std::fs::write(&file, serde_json::to_vec_pretty(&observed)?)
        .with_context(|| format!("retain {}", file.display()));
    // The kept file holds every run. The error carries a bounded excerpt.
    let mut excerpt = runs.to_string();
    if excerpt.len() > 4000 {
        let end = excerpt.floor_char_boundary(4000);
        excerpt.truncate(end);
        excerpt.push_str("...");
    }
    let error = error.context(format!(
        "{helper} run results (all in {}): {excerpt}",
        file.display()
    ));
    match kept {
        Ok(()) => Err(error),
        Err(keeping) => Err(error.context(format!("{keeping:#}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::check_with_results;
    use serde_json::{Value, json};

    #[tokio::test]
    async fn failed_run_check_retains_status_error_and_output() {
        let directory = std::env::temp_dir().join(format!("wamn-run-check-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let runs = json!([{"status":"infrastructure-failure", "fail_type":"fake-start-error",
            "result_json":{"error":"fake guest error", "output":"fake run output"}}]);
        let error = check_with_results(
            &directory,
            "fake-helper",
            async { Err::<(), _>(anyhow::anyhow!("fake timeout")) },
            async { Ok(runs.clone()) },
        )
        .await
        .unwrap_err();
        let text = format!("{error:#}");
        let kept: Value = serde_json::from_slice(
            &std::fs::read(directory.join("run-result-fake-helper.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(kept["runs"], runs);
        for expected in [
            "infrastructure-failure",
            "fake-start-error",
            "fake guest error",
            "fake run output",
            "fake timeout",
        ] {
            assert!(text.contains(expected), "missing {expected}: {text}");
            assert!(kept.to_string().contains(expected));
        }
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[tokio::test]
    async fn passing_run_check_does_not_read_results() {
        let value = check_with_results(
            std::path::Path::new("unused"),
            "passing-helper",
            async { Ok(42) },
            async { panic!("a passing check must not read run results") },
        )
        .await
        .unwrap();
        assert_eq!(value, 42);
    }
}
