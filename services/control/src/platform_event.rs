//! Map authenticated control routes to platform journal commands.

use wamn_identity_protocol::PrincipalId;
use wamn_platform_install::platform_event::EventType;
pub(crate) use wamn_platform_install::platform_event::{Command, Outcome, record};

fn event_type(operation: &str) -> Option<EventType> {
    Some(match operation {
        "env.configure" => EventType::EnvApply,
        "project_env.deactivate" => EventType::ProjectEnvDeactivate,
        "app.deploy" => EventType::Deploy,
        "app.rollback" => EventType::Rollback,
        "schema.apply" | "platform.schema" => EventType::SchemaApply,
        "schema.reset" => EventType::SchemaReset,
        "capture_gap.recover" | "capture_gap.close" => EventType::CaptureGapRepair,
        _ => return None,
    })
}

pub(crate) fn command(
    operation: &str,
    principal_id: PrincipalId,
    payload: &str,
) -> Option<Command> {
    let event_type = event_type(operation)?;
    let input: serde_json::Value = serde_json::from_str(payload).unwrap_or_default();
    if operation == "platform.schema" && input["apply"] != true {
        return None;
    }
    let project = input["project"].as_str().or_else(|| {
        // Platform schema apply can operate on an entire environment.
        let [project] = input["projects"].as_array()?.as_slice() else {
            return None;
        };
        project.as_str()
    });
    Some(Command {
        id: uuid::Uuid::new_v4(),
        event_type,
        principal_id,
        env: input["env"].as_str().and_then(|env| env.parse().ok()),
        project: project.and_then(|project| project.parse().ok()),
        app: input["app"].as_str().and_then(|app| app.parse().ok()),
        package_digest: input["release"]
            .as_str()
            .and_then(|digest| digest.parse().ok()),
        helm_revision: input["revision"]
            .as_u64()
            .and_then(|rev| i32::try_from(rev).ok()),
    })
}

#[cfg(test)]
mod tests;
