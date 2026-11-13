use super::{Triple, Value, json, operator_subject};
use anyhow::Context as _;

pub(in super::super) fn render_pat_secret(
    triple: &Triple,
    namespace: &str,
    purpose: PatPurpose,
    principal_id: &str,
    token: &str,
    prefix: &str,
    expires_at: &str,
) -> anyhow::Result<Value> {
    let subject = purpose.subject(triple)?;
    Ok(json!({
        "apiVersion": "v1",
        "kind": "Secret",
        "metadata": {
            "name": purpose.secret_name(triple),
            "namespace": namespace,
            "labels": {
                "app.kubernetes.io/managed-by": "wamn",
                "app.kubernetes.io/component": "project-env-pat",
                "wamn.org": triple.org,
                "wamn.project": triple.project,
                "wamn.env": triple.env.as_str(),
            },
            "annotations": {
                "wamn.io/credential-purpose": purpose.purpose,
                "wamn.io/principal-id": principal_id,
                "wamn.io/principal-type": "service",
                "wamn.io/principal-subject": subject,
                "wamn.io/project-role": purpose.role,
                "wamn.io/pat-prefix": prefix,
                "wamn.io/pat-expires-at": expires_at,
            },
        },
        "type": "Opaque",
        "stringData": {
            "token": token,
        },
    }))
}

pub fn parse_pat_prefix(value: &str) -> Result<String, String> {
    let valid = value.len() == 16
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'));
    if !valid {
        return Err("PAT prefix must be 16 lowercase hex digits".to_owned());
    }
    Ok(value.to_owned())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PatPurpose {
    pub(super) purpose: &'static str,
    pub(super) subject_stem: &'static str,
    pub(super) display_stem: &'static str,
    pub(super) role: &'static str,
    pub(super) secret_stem: &'static str,
}

pub const MANAGEMENT_AUTHOR: PatPurpose = PatPurpose {
    purpose: "management-author",
    subject_stem: "wamn-management-author",
    display_stem: "WAMN management author",
    role: "project-author",
    secret_stem: "wamn-pat-management-author",
};

pub(in super::super) const OPERATOR: PatPurpose = PatPurpose {
    purpose: "operator",
    subject_stem: "wamn-operator",
    display_stem: "WAMN operator",
    role: wamn_identity_protocol::ADMIN_ROLE,
    secret_stem: "wamn-pat-operator",
};

impl PatPurpose {
    pub fn subject(self, triple: &Triple) -> anyhow::Result<String> {
        if self == OPERATOR {
            return operator_subject(&triple.org, &triple.project, triple.env.as_str())
                .context("derive the canonical operator subject");
        }
        Ok(format!(
            "{}-{}--{}--{}",
            self.subject_stem, triple.org, triple.project, triple.env
        ))
    }

    pub(in super::super) fn display_name(self, triple: &Triple) -> String {
        format!(
            "{} {}/{}/{}",
            self.display_stem, triple.org, triple.project, triple.env
        )
    }

    pub(super) fn secret_name(self, triple: &Triple) -> String {
        format!(
            "{}-{}--{}--{}",
            self.secret_stem, triple.org, triple.project, triple.env
        )
    }
}
