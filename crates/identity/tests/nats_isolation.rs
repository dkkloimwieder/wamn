//! NATS isolation on the platform NATS server, with accounts and users signed
//! by the identity authority.

use std::{io::Write as _, time::Duration};

use anyhow::{Context as _, Result, ensure};
use async_nats::{Client, ConnectErrorKind, Event, ServerError, Subscriber, jetstream};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use futures_util::StreamExt as _;
use nkeys::KeyPair;
use serde::Deserialize;
use serde_json::Value;
use sqlx::PgConnection;
use tempfile::NamedTempFile;
use tokio::sync::{mpsc, oneshot};
use tokio::time::{sleep, timeout};
use wamn_envelope::encryption::Envelope;
use wamn_identity::identity::nats as key_store;
use wamn_identity::nats::{self, Bootstrap, Permissions, SubjectPermission};
use wamn_identity_protocol::nats::{SignUser, UserPurpose};
use wamn_registry::name;
use wamn_test_infrastructure::{
    containers::{self, Nats},
    database::{self, System},
};

/// The org that `database::system` provisions.
const ORG: &str = "acme";
const SUBJECT: &str = "isolation.probe";
const WAIT: Duration = Duration::from_secs(10);

#[derive(Deserialize)]
struct Settings {
    issuer: String,
    database: DatabaseSettings,
    nats: NatsSettings,
}

#[derive(Deserialize)]
struct DatabaseSettings {
    username: String,
}

#[derive(Deserialize)]
struct NatsSettings {
    file_store_bytes: i64,
}

/// The identity database, the bootstrapped authority and the NATS server that trusts it.
struct Platform {
    system: System,
    database_login: String,
    db: PgConnection,
    envelope: Envelope,
    issuer: String,
    bootstrap: Bootstrap,
    nats: Nats,
    resolver: Client,
}

async fn platform() -> Result<Platform> {
    let rendered = containers::render_platform()?;
    let settings = identity_settings(&rendered)?;
    let system = database::system().await?;
    register_envs(&system).await?;
    let mut db = connect_identity(&system, &settings.database.username).await?;
    let envelope = local_envelope()?;
    let bootstrap = key_store::bootstrap(
        &mut db,
        &settings.issuer,
        &envelope,
        settings.nats.file_store_bytes,
    )
    .await?;
    let nats = containers::start_nats(&rendered, &bootstrap.operator_include).await?;
    let resolver = nats
        .connect_options()
        .credentials(&bootstrap.system_creds)?
        .custom_inbox_prefix(&bootstrap.system_inbox)
        .connect(nats.url())
        .await?;
    Ok(Platform {
        system,
        database_login: settings.database.username,
        db,
        envelope,
        issuer: settings.issuer,
        bootstrap,
        nats,
        resolver,
    })
}

async fn register_envs(system: &System) -> Result<()> {
    let mut admin = system.admin().await?;
    let mut tx =
        wamn_postgres::provisioning_transaction::provisioning_transaction(&mut admin).await?;
    sqlx::query(
        "SELECT registry.provision_env($1, env) FROM unnest(ARRAY['dev', 'prod']) AS envs(env)",
    )
    .bind(ORG)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}

async fn edit_disk_limits(system: &System, bytes: i64) -> Result<()> {
    let mut admin = system.admin().await?;
    let mut tx =
        wamn_postgres::provisioning_transaction::provisioning_transaction(&mut admin).await?;
    sqlx::query("UPDATE registry.env SET nats_disk_bytes_per_project = $1 WHERE org = $2")
        .bind(bytes)
        .bind(ORG)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

fn identity_settings(rendered: &[Value]) -> Result<Settings> {
    let settings = rendered
        .iter()
        .find(|object| {
            object["kind"] == "ConfigMap" && object["metadata"]["name"] == "identity-settings"
        })
        .and_then(|config| config["data"]["settings.json"].as_str())
        .context("the platform chart renders no identity settings")?;
    Ok(serde_json::from_str(settings)?)
}

async fn connect_identity(system: &System, login: &str) -> Result<PgConnection> {
    let url = system.login_url(login, database::PASSWORD)?;
    wamn_postgres::connect(url.as_str()).await
}

fn local_envelope() -> Result<Envelope> {
    let mut key = NamedTempFile::new()?;
    let seed = ring::rand::generate::<[u8; 32]>(&ring::rand::SystemRandom::new())
        .map_err(|_| anyhow::anyhow!("generate the envelope key"))?;
    key.write_all(&seed.expose())?;
    Ok(wamn_key::from_file(key.path())?)
}

impl Platform {
    async fn env(&self, env: &str) -> Result<(String, Value)> {
        self.publish(env, Some(1)).await
    }

    async fn publish(&self, env: &str, project_count: Option<i32>) -> Result<(String, Value)> {
        let mut account_claims = Value::Null;
        let account = key_store::ensure_env(
            &self.bootstrap.authority,
            connect_identity(&self.system, &self.database_login).await?,
            &self.envelope,
            (&self.issuer, &ORG.parse()?, &env.parse()?),
            project_count,
            |subject, body| async move {
                let reply = self
                    .resolver
                    .request(subject, body.into())
                    .await
                    .map_err(std::io::Error::other)?;
                Ok::<_, std::io::Error>(reply.payload)
            },
            async |_, jwt| {
                account_claims = claims(&jwt).map_err(std::io::Error::other)?;
                let reply = self
                    .resolver
                    .request(nats::UPDATE_SUBJECT, jwt.into())
                    .await
                    .map_err(std::io::Error::other)?;
                let reply: Value = serde_json::from_slice(&reply.payload)?;
                if reply["data"]["code"] != 200 {
                    return Err(std::io::Error::other(format!(
                        "the resolver refused the account: {reply}"
                    )));
                }
                Ok(())
            },
        )
        .await?
        .context("expected an environment account publication")?;
        Ok((account, account_claims))
    }

    /// Sign a user with every subject in an environment account.
    async fn env_user(&self, env: &str) -> Result<(Value, String)> {
        let request = SignUser {
            org: ORG.parse()?,
            env: env.parse()?,
            purpose: UserPurpose::Workflow,
            public_key: String::new(),
        };
        self.sign_user(
            env,
            name::ENV_WORKER_ACCOUNT,
            request,
            &every_subject(),
            None,
        )
        .await
    }

    async fn sign_user(
        &self,
        caller_env: &str,
        account: &str,
        mut request: SignUser,
        permissions: &Permissions,
        expires_at: Option<i64>,
    ) -> Result<(Value, String)> {
        let key = KeyPair::new_user();
        request.public_key = key.public_key();
        let namespace = name::env_key(&request.org, &caller_env.parse()?);
        let jwt = key_store::env_user(
            connect_identity(&self.system, &self.database_login).await?,
            &self.envelope,
            &self.issuer,
            (&namespace, account),
            &request,
            &nats::User {
                public_key: &request.public_key,
                name: "isolation-probe",
                permissions,
                expires_at,
            },
        )
        .await?
        .context("the environment account is absent")?;
        Ok((claims(&jwt)?, nats::credentials(&jwt, &key)?.to_string()))
    }

    /// Sign a user with every subject in PLATFORM.
    fn platform_user(&self) -> Result<String> {
        let key = KeyPair::new_user();
        let jwt = self.bootstrap.authority.platform_user(&nats::User {
            public_key: &key.public_key(),
            name: "isolation-probe",
            permissions: &every_subject(),
            expires_at: None,
        })?;
        Ok(nats::credentials(&jwt, &key)?.to_string())
    }

    async fn connect(&self, credentials: &str) -> Result<Client> {
        Ok(self
            .nats
            .connect_options()
            .credentials(credentials)?
            .connect(self.nats.url())
            .await?)
    }

    /// Connect and report each server error message that the client receives.
    async fn connect_watched(
        &self,
        credentials: &str,
    ) -> Result<(Client, mpsc::UnboundedReceiver<String>)> {
        let (errors, received) = mpsc::unbounded_channel();
        let client = self
            .nats
            .connect_options()
            .credentials(credentials)?
            .event_callback(move |event| {
                if let Event::ServerError(ServerError::Other(message)) = event {
                    let _ = errors.send(message);
                }
                async {}
            })
            .connect(self.nats.url())
            .await?;
        Ok((client, received))
    }
}

#[tokio::test]
#[ignore = "requires: docker, bin:helm"]
async fn env_account_publication_serializes_one_env_and_retries_its_committed_revision()
-> Result<()> {
    let settings = identity_settings(&containers::render_platform()?)?;
    let system = database::system().await?;
    register_envs(&system).await?;
    let connect = || connect_identity(&system, &settings.database.username);
    let mut first = connect().await?;
    let mut second = connect().await?;
    let sibling = connect().await?;
    let mut observer = system.admin().await?;
    let first_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut first)
        .await?;
    let second_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut second)
        .await?;
    let envelope = local_envelope()?;
    let issuer = settings.issuer.as_str();
    let authority = key_store::bootstrap(
        &mut first,
        issuer,
        &envelope,
        settings.nats.file_store_bytes,
    )
    .await?
    .authority;
    let org = ORG.parse()?;
    let dev = "dev".parse()?;
    let prod = "prod".parse()?;
    let missing = |_: String, _: &'static [u8]| async { Ok::<_, std::io::Error>(Vec::<u8>::new()) };
    let (publishing, publication) = oneshot::channel();
    let (release, finish) = oneshot::channel();
    let initial = key_store::ensure_env(
        &authority,
        first,
        &envelope,
        (issuer, &org, &dev),
        Some(1),
        missing,
        async move |_, jwt| {
            publishing
                .send(jwt)
                .map_err(|_| std::io::Error::other("publication was not observed"))?;
            finish.await.map_err(std::io::Error::other)?;
            Err(std::io::Error::other(
                "the resolver did not acknowledge publication",
            ))
        },
    );
    let concurrent = async {
        let initial_jwt = timeout(WAIT, publication).await??;
        let mut retry_jwt = String::new();
        let retry = key_store::ensure_env(
            &authority,
            second,
            &envelope,
            (issuer, &org, &dev),
            Some(1),
            missing,
            async |_, jwt| {
                retry_jwt = jwt;
                Ok(())
            },
        );
        let observe = async {
            timeout(Duration::from_secs(5), async {
                loop {
                    if sqlx::query_scalar::<_, bool>("SELECT $1 = ANY(pg_blocking_pids($2))")
                        .bind(first_pid)
                        .bind(second_pid)
                        .fetch_one(&mut observer)
                        .await?
                    {
                        return Ok::<_, sqlx::Error>(());
                    }
                    sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .context("the second request for dev must wait for the first publication")??;
            let committed: i64 = sqlx::query_scalar(
                "SELECT issued_at FROM identity.nats_key
                 WHERE issuer = $1 AND kind = 'env' AND org = $2 AND env = $3",
            )
            .bind(issuer)
            .bind(org.as_str())
            .bind(dev.as_str())
            .fetch_one(&mut observer)
            .await?;
            ensure!(
                claims(&initial_jwt)?["iat"] == committed,
                "the dev revision must be committed before publication"
            );
            timeout(
                Duration::from_secs(3),
                key_store::ensure_env(
                    &authority,
                    sibling,
                    &envelope,
                    (issuer, &org, &prod),
                    Some(1),
                    missing,
                    async |_, _| Ok(()),
                ),
            )
            .await
            .context("prod must publish while the dev publication is held")??;
            release
                .send(())
                .map_err(|()| anyhow::anyhow!("dev stopped publishing early"))?;
            Ok::<_, anyhow::Error>(committed)
        };
        let (retry, committed) = tokio::join!(retry, observe);
        let committed = committed?;
        retry?;
        assert_eq!(
            claims(&retry_jwt)?["iat"],
            committed,
            "retry must publish the committed revision"
        );
        Ok::<_, anyhow::Error>(())
    };
    let (initial, concurrent) =
        Box::pin(timeout(WAIT, async { tokio::join!(initial, concurrent) })).await?;
    concurrent?;
    ensure!(initial.is_err(), "unacknowledged publication must fail");
    Ok(())
}

#[tokio::test]
#[ignore = "requires: docker, bin:helm"]
async fn concurrent_env_allocations_share_the_file_store_budget() -> Result<()> {
    let settings = identity_settings(&containers::render_platform()?)?;
    let system = database::system().await?;
    register_envs(&system).await?;
    edit_disk_limits(&system, settings.nats.file_store_bytes / 2 + 1).await?;
    let mut dev_connection = connect_identity(&system, &settings.database.username).await?;
    let prod_connection = connect_identity(&system, &settings.database.username).await?;
    let envelope = local_envelope()?;
    let issuer = settings.issuer.as_str();
    let authority = key_store::bootstrap(
        &mut dev_connection,
        issuer,
        &envelope,
        settings.nats.file_store_bytes,
    )
    .await?
    .authority;
    let org = ORG.parse()?;
    let dev = "dev".parse()?;
    let prod = "prod".parse()?;
    let allocate = async |connection, env| {
        key_store::ensure_env(
            &authority,
            connection,
            &envelope,
            (issuer, &org, env),
            Some(1),
            |_, _| async { Ok::<_, std::io::Error>(Vec::<u8>::new()) },
            async |_, _| Ok(()),
        )
        .await
    };
    let (dev, prod) = tokio::join!(
        allocate(dev_connection, &dev),
        allocate(prod_connection, &prod)
    );
    let refusal = match (dev, prod) {
        (Ok(Some(_)), Err(error)) | (Err(error), Ok(Some(_))) => error,
        outcome => anyhow::bail!("expected one allocated account and one refusal, got {outcome:?}"),
    };
    assert!(refusal.is_limit_refused(), "{refusal:?}");
    Ok(())
}

#[tokio::test]
#[ignore = "requires: docker, bin:helm"]
async fn disk_shrink_waits_for_stream_capacity_to_be_released() -> Result<()> {
    let mut platform = platform().await?;
    let (_, initial) = platform.env("dev").await?;
    let initial_disk = initial["nats"]["limits"]["disk_storage"]
        .as_i64()
        .context("signed storage limit")?;
    let (_, user) = platform.env_user("dev").await?;
    let js = jetstream::new(platform.connect(&user).await?);
    let stream = js
        .create_stream(jetstream::stream::Config {
            name: "probe".into(),
            subjects: vec![SUBJECT.into()],
            max_bytes: initial_disk,
            ..Default::default()
        })
        .await?;
    let smaller = initial_disk / 2;
    edit_disk_limits(&platform.system, smaller).await?;
    let pending = key_store::accounts_to_refresh(&mut platform.db, &platform.issuer).await?;
    assert!(pending.iter().any(|(org, env)| org == ORG && env == "dev"));

    let refused = platform
        .publish("dev", None)
        .await
        .expect_err("shrink under allocation must fail");
    assert!(
        refused
            .downcast_ref::<nats::Error>()
            .is_some_and(nats::Error::is_limit_refused),
        "{refused:#}"
    );
    js.delete_stream(stream.cached_info().config.name.as_str())
        .await?;
    let (_, shrunk) = platform.publish("dev", None).await?;
    assert_eq!(shrunk["nats"]["limits"]["disk_storage"], smaller);
    Ok(())
}

fn every_subject() -> Permissions {
    let every = || SubjectPermission {
        allow: vec![">".into()],
        deny: Vec::new(),
    };
    Permissions {
        pub_: every(),
        sub: every(),
        resp: None,
    }
}

fn claims(jwt: &str) -> Result<Value> {
    let payload = jwt.split('.').nth(1).context("a compact JWT")?;
    Ok(serde_json::from_slice(&URL_SAFE_NO_PAD.decode(payload)?)?)
}

fn credentials_jwt(credentials: &str) -> Result<&str> {
    credentials
        .lines()
        .nth(1)
        .context("a NATS credentials file")
}

fn allowed(claims: &Value, list: &str) -> Result<Vec<String>> {
    claims["nats"][list]["allow"]
        .as_array()
        .context("a signed allow list")?
        .iter()
        .map(|pattern| pattern.as_str().map(str::to_owned))
        .collect::<Option<_>>()
        .context("signed subjects are text")
}

/// A subject that `pattern` matches.
fn subject_in(pattern: &str) -> String {
    pattern
        .split('.')
        .map(|token| {
            if matches!(token, "*" | ">") {
                "probe"
            } else {
                token
            }
        })
        .collect::<Vec<_>>()
        .join(".")
}

fn matches_pattern(pattern: &str, subject: &str) -> bool {
    let mut subject = subject.split('.');
    for token in pattern.split('.') {
        match (token, subject.next()) {
            (">", Some(_)) => return true,
            ("*", Some(_)) => {}
            (token, Some(part)) if token == part => {}
            _ => return false,
        }
    }
    subject.next().is_none()
}

/// The subjects of `from` that no pattern of `allow` matches.
fn refused(allow: &[String], from: &[String]) -> Vec<String> {
    from.iter()
        .map(|pattern| subject_in(pattern))
        .filter(|subject| {
            !allow
                .iter()
                .any(|pattern| matches_pattern(pattern, subject))
        })
        .collect()
}

async fn next_payload(subscriber: &mut Subscriber) -> Result<Vec<u8>> {
    let message = tokio::time::timeout(WAIT, subscriber.next())
        .await
        .context("wait for a message")?
        .context("the subscription ended")?;
    Ok(message.payload.to_vec())
}

/// Subscribe, then wait until the server echoes a message on the subscription.
async fn subscribed(client: &Client, subject: &str) -> Result<Subscriber> {
    let mut messages = client.subscribe(subject.to_owned()).await?;
    client.publish(subject.to_owned(), "ready".into()).await?;
    ensure!(
        next_payload(&mut messages).await? == b"ready",
        "the first message on a new subscription is not its own"
    );
    Ok(messages)
}

async fn next_error(errors: &mut mpsc::UnboundedReceiver<String>) -> Result<String> {
    tokio::time::timeout(WAIT, errors.recv())
        .await
        .context("wait for a server error")?
        .context("the client closed")
}

#[tokio::test]
#[ignore = "requires: docker, bin:helm"]
async fn each_env_has_one_account_and_no_traffic_crosses_accounts() -> Result<()> {
    let platform = platform().await?;
    let (dev, _) = platform.env("dev").await?;
    let (prod, _) = platform.env("prod").await?;
    assert_ne!(dev, prod, "two environments share one account");
    let (again, _) = platform.env("dev").await?;
    assert_eq!(
        again, dev,
        "ensuring an environment again moved its account"
    );

    let (sender_claims, sender) = platform.env_user("dev").await?;
    assert_eq!(sender_claims["nats"]["issuer_account"], dev.as_str());
    let (_, peer) = platform.env_user("dev").await?;
    let (outsider_claims, outsider) = platform.env_user("prod").await?;
    assert_eq!(outsider_claims["nats"]["issuer_account"], prod.as_str());
    let sender = platform.connect(&sender).await?;
    let peer = platform.connect(&peer).await?;
    let outsider = platform.connect(&outsider).await?;
    let mut peer_messages = subscribed(&peer, SUBJECT).await?;
    let mut outsider_messages = subscribed(&outsider, SUBJECT).await?;

    sender.publish(SUBJECT, "dev".into()).await?;
    // The server routes a message to every subscriber at once, so when the peer
    // has it, a copy for the outsider would be queued before the outsider's own.
    assert_eq!(next_payload(&mut peer_messages).await?, b"dev");
    outsider.publish(SUBJECT, "prod".into()).await?;
    assert_eq!(
        next_payload(&mut outsider_messages).await?,
        b"prod",
        "a message from another environment's account crossed into this one"
    );
    Ok(())
}

#[tokio::test]
#[ignore = "requires: docker, bin:helm"]
async fn runtime_operator_user_holds_only_its_signed_subjects() -> Result<()> {
    let platform = platform().await?;
    let operator_claims = claims(credentials_jwt(&platform.bootstrap.runtime_operator_creds)?)?;
    let publish = allowed(&operator_claims, "pub")?;
    let subscribe = allowed(&operator_claims, "sub")?;
    let (operator, mut errors) = platform
        .connect_watched(&platform.bootstrap.runtime_operator_creds)
        .await?;
    let witness = platform.connect(&platform.platform_user()?).await?;

    for pattern in &publish {
        let subject = subject_in(pattern);
        let mut messages = subscribed(&witness, &subject).await?;
        operator.publish(subject, "allowed".into()).await?;
        assert_eq!(next_payload(&mut messages).await?, b"allowed");
    }
    // The server reads one connection in order, so a marker that the operator
    // publishes after a subscription shows that the server has the subscription.
    let marker = subject_in(publish.first().context("an empty publish list")?);
    let mut markers = subscribed(&witness, &marker).await?;
    for pattern in &subscribe {
        let subject = subject_in(pattern);
        let mut messages = operator.subscribe(subject.clone()).await?;
        operator.publish(marker.clone(), "marker".into()).await?;
        assert_eq!(next_payload(&mut markers).await?, b"marker");
        witness.publish(subject, "allowed".into()).await?;
        assert_eq!(next_payload(&mut messages).await?, b"allowed");
    }

    let refused_publish = refused(&publish, &subscribe);
    ensure!(
        !refused_publish.is_empty(),
        "no subject outside the publish list"
    );
    for subject in refused_publish {
        operator.publish(subject.clone(), "refused".into()).await?;
        assert_eq!(
            next_error(&mut errors).await?,
            format!("Permissions Violation for Publish to \"{subject}\"")
        );
    }
    let refused_subscribe = refused(&subscribe, &publish);
    ensure!(
        !refused_subscribe.is_empty(),
        "no subject outside the subscribe list"
    );
    for subject in refused_subscribe {
        let _refused = operator.subscribe(subject.clone()).await?;
        assert_eq!(
            next_error(&mut errors).await?,
            format!("Permissions Violation for Subscription to \"{subject}\"")
        );
    }
    Ok(())
}

#[tokio::test]
#[ignore = "requires: docker, bin:helm"]
async fn env_account_limits_refuse_the_next_connection_and_subscription() -> Result<()> {
    let platform = platform().await?;
    let (_, account) = platform.env("dev").await?;
    let limit = |name: &str| {
        account["nats"]["limits"][name]
            .as_u64()
            .with_context(|| format!("a signed {name} limit"))
    };
    let (_, user) = platform.env_user("dev").await?;

    let (client, mut errors) = platform.connect_watched(&user).await?;
    let mut subscriptions = Vec::new();
    for index in 0..limit("subs")? {
        subscriptions.push(client.subscribe(format!("{SUBJECT}.{index}")).await?);
    }
    // The server counts subscriptions in order, so the last one it accepted
    // shows that it accepted all of them.
    let last = subscriptions
        .last_mut()
        .context("the account allows no subscription")?;
    client
        .publish(format!("{SUBJECT}.{}", limit("subs")? - 1), "last".into())
        .await?;
    assert_eq!(next_payload(last).await?, b"last");
    subscriptions.push(client.subscribe(format!("{SUBJECT}.over")).await?);
    assert_eq!(
        next_error(&mut errors).await?.to_lowercase(),
        "maximum subscriptions exceeded"
    );

    let mut clients = vec![client];
    while clients.len() < usize::try_from(limit("conn")?)? {
        clients.push(platform.connect(&user).await?);
    }
    let error = platform
        .connect(&user)
        .await
        .expect_err("a connection over the account limit was accepted");
    let error = error
        .downcast_ref::<async_nats::ConnectError>()
        .context("a NATS connect error")?;
    assert_eq!(error.kind(), ConnectErrorKind::Io);
    let Some(ServerError::Other(reason)) =
        std::error::Error::source(error).and_then(|source| source.downcast_ref::<ServerError>())
    else {
        anyhow::bail!("the server did not refuse the connection: {error}");
    };
    assert_eq!(
        reason.to_lowercase(),
        "maximum account active connections exceeded"
    );
    Ok(())
}

async fn register_projects(platform: &Platform) -> Result<()> {
    let mut admin = platform.system.admin().await?;
    let mut tx =
        wamn_postgres::provisioning_transaction::provisioning_transaction(&mut admin).await?;
    sqlx::query(
        "INSERT INTO registry.project (org, id)
         VALUES ($1, 'billing'), ($1, 'inventory')",
    )
    .bind(ORG)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "INSERT INTO registry.project_env (org, env, project, secret_name, instance_suffix)
         SELECT p.org, e.env, p.id, p.id || '-db', '12345678'
         FROM registry.project p JOIN registry.env e USING (org) WHERE p.org = $1",
    )
    .bind(ORG)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}

fn recovery(env: &str, project: &str) -> Result<SignUser> {
    Ok(SignUser {
        org: ORG.parse()?,
        env: env.parse()?,
        purpose: UserPurpose::Recovery {
            project: project.parse()?,
        },
        public_key: String::new(),
    })
}

#[tokio::test]
#[ignore = "requires: docker, bin:helm"]
async fn recovery_account_cannot_sign_for_another_project_or_env() -> Result<()> {
    let platform = platform().await?;
    register_projects(&platform).await?;
    platform.env("dev").await?;
    platform.env("prod").await?;
    let org = ORG.parse()?;
    let project = "billing".parse()?;
    let dev = "dev".parse()?;
    let account = name::project_operation_account(&org, &project, &dev);
    let now: i64 = std::time::UNIX_EPOCH.elapsed()?.as_secs().try_into()?;
    for (caller_env, request) in [
        ("dev", recovery("dev", "inventory")?),
        ("dev", recovery("prod", "billing")?),
    ] {
        let error = platform
            .sign_user(
                caller_env,
                &account,
                request,
                &every_subject(),
                Some(now + 60),
            )
            .await
            .expect_err("identity signed outside the authenticated scope");
        assert!(
            error
                .downcast_ref::<nats::Error>()
                .is_some_and(nats::Error::is_scope_refused)
        );
    }
    Ok(())
}

#[tokio::test]
#[ignore = "requires: docker, bin:helm"]
async fn revocation_disconnects_the_project_and_preserves_its_sibling_and_account() -> Result<()> {
    let platform = platform().await?;
    register_projects(&platform).await?;
    let (account, _) = platform.publish("dev", Some(2)).await?;
    let project_creds = async |project: &str| -> Result<(Value, String)> {
        let request = SignUser {
            purpose: UserPurpose::Cdc {
                project: project.parse()?,
            },
            ..recovery("dev", project)?
        };
        platform
            .sign_user(
                "dev",
                name::ENV_WORKER_ACCOUNT,
                request,
                &every_subject(),
                None,
            )
            .await
    };
    let (claims, creds) = project_creds("billing").await?;
    let (_, sibling) = project_creds("inventory").await?;
    let sibling_connection = platform.connect(&sibling).await?;
    let mut messages = subscribed(&sibling_connection, SUBJECT).await?;
    let (events, mut disconnected) = mpsc::channel(1);
    let client = platform
        .nats
        .connect_options()
        .credentials(&creds)?
        .event_callback(move |event| {
            if matches!(event, Event::Disconnected) {
                let _ = events.try_send(());
            }
            async {}
        })
        .connect(platform.nats.url())
        .await?;
    client.flush().await?;
    let mut admin = platform.system.admin().await?;
    let mut tx =
        wamn_postgres::provisioning_transaction::provisioning_transaction(&mut admin).await?;
    sqlx::query("UPDATE registry.project_env SET active = false WHERE org = $1 AND env = 'dev' AND project = 'billing'")
        .bind(ORG).execute(&mut *tx).await?;
    tx.commit().await?;
    let (retained, revision) = platform.publish("dev", None).await?;
    assert_eq!(retained, account);
    assert_eq!(
        revision["nats"]["revocations"][claims["sub"].as_str().context("user subject")?],
        claims["iat"]
    );
    timeout(WAIT, disconnected.recv())
        .await?
        .context("wait for NATS to disconnect the revoked user")?;
    assert!(
        platform.connect(&creds).await.is_err(),
        "a revoked key reconnected"
    );
    let sibling = platform.connect(&sibling).await?;
    sibling.publish(SUBJECT, "still active".into()).await?;
    assert_eq!(next_payload(&mut messages).await?, b"still active");
    Ok(())
}

#[tokio::test]
#[ignore = "requires: docker, bin:helm"]
async fn nats_refuses_a_job_jwt_at_its_deadline() -> Result<()> {
    let platform = platform().await?;
    platform.env("dev").await?;
    let now: i64 = std::time::UNIX_EPOCH.elapsed()?.as_secs().try_into()?;
    let deadline = now + 3;
    let request = SignUser {
        purpose: UserPurpose::StreamAdmin,
        ..recovery("dev", "unused")?
    };
    let (_, creds) = platform
        .sign_user(
            "dev",
            name::ENV_WORKER_ACCOUNT,
            request,
            &every_subject(),
            Some(deadline),
        )
        .await?;
    platform.connect(&creds).await?.flush().await?;
    let expires = std::time::UNIX_EPOCH + Duration::from_secs(deadline.try_into()?);
    sleep(
        expires
            .duration_since(std::time::SystemTime::now())
            .unwrap_or_default()
            + Duration::from_secs(1),
    )
    .await;
    assert!(
        platform.connect(&creds).await.is_err(),
        "an expired Job key reconnected"
    );
    Ok(())
}
