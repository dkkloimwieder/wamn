//! Replace declared result UUIDs atomically, without exposing partial results.

use std::time::Duration;

use anyhow::Context as _;
use serde_json::Value;
use uuid::Uuid;
use wamn_catalog::{SignedFileField, SignedFileReads, SignedFileResultClass};
use wash_runtime::wasmtime::component::Val;

use super::native_call::NativeOutcome;

/// A result transformation failure retains its original cause and result kind.
#[derive(Debug)]
pub(super) struct SignedResultError {
    kind: &'static str,
    source: anyhow::Error,
}
impl std::fmt::Display for SignedResultError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "sign {} operation result: {}", self.kind, self.source)
    }
}
impl std::error::Error for SignedResultError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.source.as_ref())
    }
}

/// Own the result until every signed read succeeds. Neither failure nor
/// cancellation publishes a result containing only some of its URLs.
pub(super) async fn sign_result<F, Fut>(
    outcome: NativeOutcome,
    reads: &SignedFileReads,
    sign: F,
) -> Result<NativeOutcome, SignedResultError>
where
    F: FnMut(Uuid, Duration) -> Fut + Send,
    Fut: Future<Output = anyhow::Result<String>> + Send,
{
    let kind = match &outcome {
        NativeOutcome::Json(_) => "JSON",
        NativeOutcome::Typed(_) => "typed",
    };
    sign_result_inner(outcome, reads, sign)
        .await
        .map_err(|source| SignedResultError { kind, source })
}

async fn sign_result_inner<F, Fut>(
    outcome: NativeOutcome,
    reads: &SignedFileReads,
    mut sign: F,
) -> anyhow::Result<NativeOutcome>
where
    F: FnMut(Uuid, Duration) -> Fut + Send,
    Fut: Future<Output = anyhow::Result<String>> + Send,
{
    match outcome {
        NativeOutcome::Json(Ok(mut emission)) => {
            anyhow::ensure!(
                emission.port.as_deref().is_none_or(|port| port == "main"),
                "signed operation returned another output port"
            );
            let mut payload: Value = serde_json::from_str(&emission.payload)
                .context("decode operation result for file signing")?;
            for field in &reads.fields {
                let paths = json_paths(&payload, reads.class, field)?;
                let lifetime = Duration::from_secs(field.read.lifetime_seconds()?);
                for path in paths {
                    let target = payload
                        .pointer_mut(&path)
                        .context("signed result field disappeared")?;
                    let id = target
                        .as_str()
                        .context("signed result field is not a file UUID")?
                        .parse()?;
                    *target = Value::String(sign(id, lifetime).await?);
                }
            }
            emission.payload = serde_json::to_string(&payload)?;
            Ok(NativeOutcome::Json(Ok(emission)))
        }
        NativeOutcome::Typed(mut value) => {
            for field in &reads.fields {
                let paths = typed_paths(&value, reads.class, field)?;
                let lifetime = Duration::from_secs(field.read.lifetime_seconds()?);
                for path in paths {
                    let target = typed_at_mut(&mut value, &path)?;
                    let Val::String(id) = target else {
                        anyhow::bail!("signed typed result is not a file UUID")
                    };
                    let url = sign(id.parse()?, lifetime).await?;
                    *target = Val::String(url);
                }
            }
            Ok(NativeOutcome::Typed(value))
        }
        refused @ NativeOutcome::Json(Err(_)) => Ok(refused),
    }
}

fn json_paths(
    value: &Value,
    class: SignedFileResultClass,
    field: &SignedFileField,
) -> anyhow::Result<Vec<String>> {
    let mut paths = Vec::new();
    for (index, element) in value
        .as_array()
        .context("operation result is not an outcome list")?
        .iter()
        .enumerate()
    {
        if let Some(error) = element.get("error") {
            anyhow::ensure!(
                error.is_object() && element.get("value").is_none(),
                "operation result mixes a value with a refusal"
            );
            continue;
        }
        let row = element
            .get("value")
            .context("operation outcome has no value")?;
        let base = format!("/{index}/value");
        match class {
            SignedFileResultClass::One => {
                json_field(row, &base, &field.path, field.nullable, &mut paths)?;
            }
            SignedFileResultClass::OptionalOne => {
                if !row.is_null() {
                    json_field(row, &base, &field.path, field.nullable, &mut paths)?;
                }
            }
            SignedFileResultClass::BoundedList | SignedFileResultClass::Page => {
                for (i, row) in row
                    .get("rows")
                    .and_then(Value::as_array)
                    .context("signed result has no rows")?
                    .iter()
                    .enumerate()
                {
                    json_field(
                        row,
                        &format!("{base}/rows/{i}"),
                        &field.path,
                        field.nullable,
                        &mut paths,
                    )?;
                }
            }
        }
    }
    Ok(paths)
}

fn json_field(
    value: &Value,
    base: &str,
    path: &str,
    nullable: bool,
    paths: &mut Vec<String>,
) -> anyhow::Result<()> {
    if path.is_empty() {
        if !value.is_null() || !nullable {
            paths.push(base.into());
        }
        return Ok(());
    }
    let (member, rest) = path.split_once('.').unwrap_or((path, ""));
    let name = member.strip_suffix("[]").unwrap_or(member);
    let value = value.get(name).context("signed result field is missing")?;
    let base = format!("{base}/{name}");
    if member.ends_with("[]") {
        for (i, element) in value
            .as_array()
            .context("signed result field is not a list")?
            .iter()
            .enumerate()
        {
            json_field(element, &format!("{base}/{i}"), rest, nullable, paths)?;
        }
    } else {
        json_field(value, &base, rest, nullable, paths)?;
    }
    Ok(())
}

// Indexes address owned WIT values. Records use WIT's kebab-case spelling,
// while package field paths retain their JSON snake_case spelling.
fn typed_paths(
    value: &Val,
    class: SignedFileResultClass,
    field: &SignedFileField,
) -> anyhow::Result<Vec<Vec<usize>>> {
    let Val::Result(result) = value else {
        anyhow::bail!("typed operation result is not a result")
    };
    let elements = match result {
        Err(_) => return Ok(Vec::new()),
        Ok(Some(elements)) => elements,
        Ok(None) => anyhow::bail!("typed operation returned no outcome list"),
    };
    let Val::List(elements) = elements.as_ref() else {
        anyhow::bail!("typed operation result is not an outcome list")
    };
    let mut paths = Vec::new();
    for (i, element) in elements.iter().enumerate() {
        let (index, outcome) = typed_member(element, "outcome")?;
        let Val::Result(result) = outcome else {
            anyhow::bail!("typed outcome is not a result")
        };
        let row = match result {
            Err(_) => continue,
            Ok(Some(row)) => row,
            Ok(None) => anyhow::bail!("typed operation outcome has no value"),
        };
        let base = vec![0, i, index, 0];
        match class {
            SignedFileResultClass::One => {
                typed_field(row, base, &field.path, field.nullable, &mut paths)?;
            }
            SignedFileResultClass::OptionalOne => {
                let (index, value) = typed_member(row, "value")?;
                let Val::Option(value) = value else {
                    anyhow::bail!("typed optional result is not an option")
                };
                if let Some(value) = value {
                    let mut base = base;
                    base.extend([index, 0]);
                    typed_field(value, base, &field.path, field.nullable, &mut paths)?;
                }
            }
            SignedFileResultClass::BoundedList | SignedFileResultClass::Page => {
                let (index, rows) = typed_member(row, "rows")?;
                let Val::List(rows) = rows else {
                    anyhow::bail!("typed result rows are not a list")
                };
                for (j, row) in rows.iter().enumerate() {
                    let mut base = base.clone();
                    base.extend([index, j]);
                    typed_field(row, base, &field.path, field.nullable, &mut paths)?;
                }
            }
        }
    }
    Ok(paths)
}

fn typed_member<'a>(value: &'a Val, name: &str) -> anyhow::Result<(usize, &'a Val)> {
    let Val::Record(fields) = value else {
        anyhow::bail!("signed typed result is not a record")
    };
    fields
        .iter()
        .enumerate()
        .find_map(|(i, (key, value))| (key == &name.replace('_', "-")).then_some((i, value)))
        .context("signed typed result field is missing")
}

fn typed_field(
    value: &Val,
    mut base: Vec<usize>,
    path: &str,
    nullable: bool,
    paths: &mut Vec<Vec<usize>>,
) -> anyhow::Result<()> {
    if path.is_empty() {
        if nullable {
            let Val::Option(value) = value else {
                anyhow::bail!("nullable typed signed field is not an option")
            };
            if value.is_some() {
                base.push(0);
                paths.push(base);
            }
        } else {
            paths.push(base);
        }
        return Ok(());
    }
    let (member, rest) = path.split_once('.').unwrap_or((path, ""));
    let name = member.strip_suffix("[]").unwrap_or(member);
    let (index, value) = typed_member(value, name)?;
    base.push(index);
    if member.ends_with("[]") {
        let Val::List(elements) = value else {
            anyhow::bail!("signed typed result field is not a list")
        };
        for (i, element) in elements.iter().enumerate() {
            let mut base = base.clone();
            base.push(i);
            typed_field(element, base, rest, nullable, paths)?;
        }
    } else {
        typed_field(value, base, rest, nullable, paths)?;
    }
    Ok(())
}

fn typed_at_mut<'a>(mut value: &'a mut Val, path: &[usize]) -> anyhow::Result<&'a mut Val> {
    for &i in path {
        value = match value {
            Val::Result(Ok(Some(value))) | Val::Option(Some(value)) if i == 0 => value,
            Val::Record(fields) => {
                &mut fields
                    .get_mut(i)
                    .context("typed result record field disappeared")?
                    .1
            }
            Val::List(elements) => elements
                .get_mut(i)
                .context("typed result list request element disappeared")?,
            _ => anyhow::bail!("typed signed result path is invalid"),
        };
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::{NativeOutcome, SignedFileReads, Val, sign_result};
    use serde_json::json;
    use std::time::Duration;
    use wamn_engine::operation::node_types;

    const ID: &str = "f170fc66-34a9-4b10-97e7-14ca65ea2ed0";

    fn reads(class: &str) -> SignedFileReads {
        serde_json::from_value(json!({"class": class, "fields": [
            {"path": "file_id", "nullable": true, "read": {"lifetime_seconds": 12}}
        ]}))
        .unwrap()
    }
    fn emission(value: &serde_json::Value) -> NativeOutcome {
        NativeOutcome::Json(Ok(node_types::Emission {
            port: Some("main".into()),
            payload: value.to_string(),
        }))
    }
    fn typed(class: &str) -> Val {
        let row = Val::Record(vec![(
            "file-id".into(),
            Val::Option(Some(Box::new(Val::String(ID.into())))),
        )]);
        let value = match class {
            "one" => row,
            "optional_one" => Val::Record(vec![("value".into(), Val::Option(Some(Box::new(row))))]),
            _ => Val::Record(vec![("rows".into(), Val::List(vec![row]))]),
        };
        let outcome = Val::Result(Ok(Some(Box::new(value))));
        let element = Val::Record(vec![("outcome".into(), outcome)]);
        Val::Result(Ok(Some(Box::new(Val::List(vec![element])))))
    }

    #[tokio::test]
    async fn json_and_typed_results_sign_each_result_class() {
        for class in ["one", "optional_one", "bounded_list", "page"] {
            let row = json!({"file_id": ID});
            let value = if class == "bounded_list" || class == "page" {
                json!({"rows": [row]})
            } else {
                row
            };
            let input = json!([{"value": value}, {"error": {"code": "missing"}}]);
            let signed = sign_result(emission(&input), &reads(class), |id, lifetime| async move {
                assert_eq!(id.to_string(), ID);
                assert_eq!(lifetime, Duration::from_secs(12));
                Ok("https://files.test/signed".into())
            })
            .await
            .unwrap()
            .into_json()
            .unwrap()
            .unwrap();
            assert!(signed.payload.contains("https://files.test/signed"));
            assert!(!signed.payload.contains(ID));
            let value = sign_result(
                NativeOutcome::Typed(typed(class)),
                &reads(class),
                |id, lifetime| async move {
                    assert_eq!(id.to_string(), ID);
                    assert_eq!(lifetime, Duration::from_secs(12));
                    Ok("https://files.test/signed".into())
                },
            )
            .await
            .unwrap();
            assert!(format!("{value:?}").contains("https://files.test/signed"));
            assert!(!format!("{value:?}").contains(ID));
        }
    }

    #[tokio::test]
    async fn a_later_signing_failure_refuses_the_whole_result() {
        let input = json!([{"value": {"file_id": ID}}, {"value": {"file_id": ID}}]);
        let mut count = 0;
        let result = sign_result(emission(&input), &reads("one"), |_, _| {
            count += 1;
            let current = count;
            async move {
                if current == 1 {
                    Ok("https://files.test/partial".into())
                } else {
                    anyhow::bail!("signing refused")
                }
            }
        })
        .await;
        assert_eq!(count, 2);
        assert_eq!(
            result.unwrap_err().to_string(),
            "sign JSON operation result: signing refused"
        );
        assert_eq!(input[0]["value"]["file_id"], ID);
    }

    #[tokio::test]
    async fn typed_failure_drops_the_entire_owned_result() {
        let mut value = typed("one");
        let Val::Result(Ok(Some(elements))) = &mut value else {
            panic!("fixture outer result")
        };
        let Val::List(elements) = elements.as_mut() else {
            panic!("fixture outcomes")
        };
        elements.push(elements[0].clone());
        let mut calls = 0;
        let result = sign_result(NativeOutcome::Typed(value), &reads("one"), |_, _| {
            calls += 1;
            let call = calls;
            async move {
                if call == 1 {
                    Ok("https://files.test/partial".into())
                } else {
                    anyhow::bail!("provider refused")
                }
            }
        })
        .await;
        assert_eq!(calls, 2);
        assert_eq!(
            result.unwrap_err().to_string(),
            "sign typed operation result: provider refused"
        );
    }

    #[tokio::test]
    async fn null_optional_and_refused_results_do_not_sign() {
        for input in [
            json!([{"value": null}]),
            json!([{"value": {"file_id": null}}]),
            json!([{"error": {"code": "missing"}}]),
        ] {
            sign_result(emission(&input), &reads("optional_one"), |_, _| async {
                panic!("nulls and refusals must not sign")
            })
            .await
            .unwrap();
        }
        for input in [
            json!([{"value": {}}]),
            json!([{"value": {"file_id": 42}}]),
            json!([{"value": {"file_id": "not-a-uuid"}}]),
            json!([{"value": {"file_id": ID}, "error": {"code": "missing"}}]),
        ] {
            assert!(
                sign_result(emission(&input), &reads("one"), |_, _| async {
                    panic!("invalid results must not reach storage")
                })
                .await
                .is_err()
            );
        }
    }
}
