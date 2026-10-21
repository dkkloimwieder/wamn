//! Prepare the database logins and native sidecar of a wired release Job.

use crate::environment::document::EnvironmentDocument;

use super::install::DATABASE_HOST;
use super::job::{Access, JobContainer, JobRequest, JobSecret, PolicyRule, SecretMount};

// Only database credentials use these files. The main container mints the PAT
// after preparation, and its process never writes the PAT.
const PREPARE: &str = r#"prepare() {
  umask 077
  scratch=$(mktemp -d)
  trap 'rm -rf "$scratch"' EXIT
  waited=0
  while ! uid=$(kubectl -n identity get job "$JOB_NAME" -o jsonpath='{.metadata.uid}'); do
    test "$waited" -lt 120 || return 1
    sleep 1
    waited=$((waited + 1))
  done
  test -n "$uid" || return 1
  for family in identity-reader control-author management-admitter; do
    if [ "$family" = management-admitter ]; then
      export WAMN_TARGET_ADMIN_DATABASE_URL="$WAMN_PG_ADMIN_URL"
    else
      unset WAMN_TARGET_ADMIN_DATABASE_URL
    fi
    /usr/local/bin/wamn-ctl provision-project-env \
      --org "$ORG" --project "$PROJECT" --env "$ENVIRONMENT" --tenant "$TENANT" \
      --namespace identity --db-host "$DATABASE_HOST" --db-port 5432 \
      "--prepare-$family-generation" a "--emit-$family-secret" "$scratch/secret.json" || return 1
    patch=$(printf '[{"op":"replace","path":"/metadata","value":{"name":"%s-%s","namespace":"identity","ownerReferences":[{"apiVersion":"batch/v1","kind":"Job","name":"%s","uid":"%s"}]}}]' "$JOB_NAME" "$family" "$JOB_NAME" "$uid")
    kubectl patch --local --type=json -f "$scratch/secret.json" -p "$patch" -o json > "$scratch/owned.json" || return 1
    kubectl create -f "$scratch/owned.json" || return 1
    rm "$scratch/secret.json" "$scratch/owned.json"
  done
}
prepare >/dev/null 2>/dev/null || { echo 'prepare wiring gate database logins failed' >&2; exit 1; }
"#;

/// Configure one pod with database preparation, a native gate sidecar, and certificate mounts.
///
/// Database login values stay in the setup container and Job-owned Secrets.
/// Existing administrator inputs use the Job runner's credential references.
pub fn configure(
    request: &mut JobRequest,
    environment: &EnvironmentDocument,
    secrets: &[JobSecret],
) {
    let tenant = format!(
        "{}--{}--{}",
        environment.org, environment.project, environment.env
    );
    request.calls_kubernetes = true;
    request.permissions.push(Access {
        namespace: "identity".into(),
        rules: vec![
            PolicyRule {
                api_groups: vec!["batch".into()],
                resources: vec!["jobs".into()],
                verbs: vec!["get".into()],
            },
            PolicyRule {
                api_groups: vec![String::new()],
                resources: vec!["secrets".into()],
                verbs: vec!["create".into()],
            },
        ],
    });
    request.setup = Some(JobContainer {
        command: vec!["/bin/sh".into(), "-ec".into()],
        arguments: vec![PREPARE.into()],
        environment: [
            ("JOB_NAME".into(), request.name.clone()),
            ("ORG".into(), environment.org.clone()),
            ("PROJECT".into(), environment.project.clone()),
            ("ENVIRONMENT".into(), environment.env.clone()),
            ("TENANT".into(), tenant.clone()),
            ("DATABASE_HOST".into(), DATABASE_HOST.into()),
        ]
        .into(),
        secret_environment: secrets
            .iter()
            .map(|secret| {
                (
                    secret.environment.clone(),
                    (secret.name.clone(), secret.key.clone()),
                )
            })
            .collect(),
    });
    let mut sidecar = JobContainer {
        command: vec!["/usr/local/bin/wamn-scenario-worker".into()],
        arguments: vec!["serve".into(), "--bind".into(), "127.0.0.1:18090".into()],
        environment: request.environment.clone(),
        secret_environment: [
            ("WAMN_SYSTEM_URL", "identity-reader"),
            ("WAMN_CONTROL_AUTHORING_PG_URL", "control-author"),
            ("WAMN_MANAGEMENT_ADMISSION_PG_URL", "management-admitter"),
        ]
        .map(|(variable, family)| {
            (
                variable.into(),
                (format!("{}-{family}", request.name), "url".into()),
            )
        })
        .into(),
    };
    sidecar.environment.extend([
        ("WAMN_MANAGEMENT_ORG".into(), environment.org.clone()),
        (
            "WAMN_MANAGEMENT_PROJECT".into(),
            environment.project.clone(),
        ),
        (
            "WAMN_MANAGEMENT_ENVIRONMENT".into(),
            environment.env.clone(),
        ),
        ("WAMN_MANAGEMENT_TENANT".into(), tenant),
    ]);
    request.sidecar = Some(sidecar);
    request.secret_mounts.extend([
        SecretMount {
            name: format!("operator-{}", environment.org),
            path: "/etc/wamn/operator".into(),
            keys: vec!["tls.crt".into(), "tls.key".into()],
        },
        SecretMount {
            name: "identity-tls".into(),
            path: "/etc/wamn/identity-ca".into(),
            keys: vec!["ca.crt".into()],
        },
    ]);
    request.environment.extend([
        (
            "WAMN_PAT_ISSUER".into(),
            "https://identity.identity.svc.cluster.local".into(),
        ),
        (
            "WAMN_PAT_CLIENT_CERT".into(),
            "/etc/wamn/operator/tls.crt".into(),
        ),
        (
            "WAMN_PAT_CLIENT_KEY".into(),
            "/etc/wamn/operator/tls.key".into(),
        ),
        (
            "WAMN_PAT_SERVER_CA".into(),
            "/etc/wamn/identity-ca/ca.crt".into(),
        ),
    ]);
}
