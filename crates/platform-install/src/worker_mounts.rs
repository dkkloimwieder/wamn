//! Database staging for an operation Job.

use serde_json::{Value, json};
use wamn_kubernetes::job;

/// Mount the worker's credential staging directory.
pub fn mount_database_files(job: &mut Value) {
    job["spec"]["template"]["spec"]["volumes"]
        .as_array_mut()
        .expect("worker volumes")
        .push(json!({"name":"database-files","emptyDir":{"medium":"Memory"}}));
    job::container(job)["volumeMounts"]
        .as_array_mut()
        .expect("worker volume mounts")
        .push(json!({"name":"database-files","mountPath":wamn_postgres::database::DATABASE_DIR}));
}
