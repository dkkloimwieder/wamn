//! Broker users open the authority memory watch.
use serde_json::json;
use std::time::Duration;
use wamn_broker::authorization::test_util::{
    Account, Authorization, Permissions, Subjects, control_user, render,
};
use wamn_registry::Triple;
fn document(project: &str) -> Triple {
    Triple::new("test", "dev".parse().unwrap(), project)
}
/// The control host and an environment host open their auth memory
/// watches as their own users on a real server (docs/plan/alignment.md
/// R5, R7, A7). Each user gets a password here in place of its
/// certificate, and the permissions are the rendered ones.
#[tokio::test]
#[ignore = "requires: nats, env:WAMN_NATIVE_C_NATS_BIN"]
async fn the_control_and_environment_users_open_their_memory_watches() {
    let binary = std::env::var_os("WAMN_NATIVE_C_NATS_BIN")
        .expect("set WAMN_NATIVE_C_NATS_BIN to a nats-server executable");
    let directory = std::env::temp_dir().join(format!("wamn-broker-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&directory).unwrap();
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let mut authorization = Authorization { users: Vec::new() };
    render(&document("one"), &mut authorization);
    authorization.users.push(control_user("test").0);
    authorization.users.push(Account {
        user: "admin".into(),
        password: None,
        permissions: Permissions {
            publish: Subjects {
                allow: vec![">".into()],
                deny: Vec::new(),
            },
            subscribe: Subjects {
                allow: vec![">".into()],
                deny: Vec::new(),
            },
        },
    });
    for user in &mut authorization.users {
        user.password = Some("p".into());
    }
    let configuration = directory.join("nats.conf");
    std::fs::write(
        &configuration,
        format!(
            "listen: \"127.0.0.1:{port}\"\njetstream {{ store_dir: {:?} }}\nauthorization: {}\n",
            directory.join("store"),
            json!(authorization)
        ),
    )
    .unwrap();
    let mut server = std::process::Command::new(binary)
        .arg("-c")
        .arg(&configuration)
        .spawn()
        .unwrap();
    let url = format!("nats://127.0.0.1:{port}");
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let admin = loop {
        match async_nats::ConnectOptions::with_user_and_password("admin".into(), "p".into())
            .connect(url.clone())
            .await
        {
            Ok(client) => break client,
            Err(error) => assert!(std::time::Instant::now() < deadline, "{error}"),
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    wamn_broker::kv_memory::create_buckets(&admin)
        .await
        .unwrap();
    let project_env_key = wamn_registry::project_env_key("test", "one", &"dev".parse().unwrap());
    for (user, target) in [
        (
            wamn_registry::environment_nats_user(&project_env_key),
            Some(&project_env_key),
        ),
        (wamn_registry::control_nats_user("test"), None),
    ] {
        eprintln!("{user} opens its auth memory watch");
        let client = async_nats::ConnectOptions::with_user_and_password(user.clone(), "p".into())
            .custom_inbox_prefix(wamn_registry::name::nats_subject(&[
                &wamn_registry::name::nats_token("_INBOX_", (user).as_ref(), ""),
            ]))
            .connect(url.clone())
            .await
            .unwrap();
        let cache = std::sync::Arc::new(wamn_authority::auth_cache::AuthCache::default());
        wamn_authority::auth_cache::test_util::watched(&client, target, cache).await;
    }
    let _ = server.kill();
    let _ = server.wait();
    let _ = std::fs::remove_dir_all(&directory);
}
