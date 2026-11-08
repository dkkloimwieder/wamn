//! Application admission during upgrade, rollback, and broker redelivery.
#[path = "support/schedule_fixture.rs"]
pub mod support;
use support::*;

mod event_batch {
    use super::*;
    use futures_util::StreamExt as _;
    use wamn_runtime::plugins::wamn_jetstream::WamnJetstreamConfig;

    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "requires: env:WAMN_NATS_URL, env:WAMN_NATS_TLS_CA, env:WAMN_NATS_TLS_CERT, env:WAMN_NATS_TLS_KEY"]
    async fn applications_admit_independently_across_upgrade_rollback_and_redelivery() {
        wamn_test_postgres::require_ignored_prerequisites(
            "requires: env:WAMN_NATS_URL, env:WAMN_NATS_TLS_CA, env:WAMN_NATS_TLS_CERT, env:WAMN_NATS_TLS_KEY",
        );
        let _lock = wamn_test_postgres::lock();
        let fixture = fixture().await;
        fixture.grant_workflow_user("reader").await;
        let txid = changed_order(&fixture).await;
        event(&fixture, "first", 0, Some("reader")).await;
        fixture.admin.execute(
            "INSERT INTO catalog.workflows (env_key, environment, application, workflow_id, version, graph_json, wiring_hash, trigger, source_package, entity, ops, enable_sequence, execution_type, execution_role, enabled, created_by) \
             SELECT env_key, environment, 'billing', workflow_id, version, graph_json, wiring_hash, trigger, source_package, entity, ops, enable_sequence, execution_type, execution_role, enabled, created_by FROM catalog.workflows WHERE application = $1",
            &[&APP],
        ).await.unwrap();
        let upgraded = release_variant(&fixture, APP, b"1251").await;
        let billing = release_variant(&fixture, "billing", b"1252").await;
        let mut retired = serde_json::to_value(upgraded.manifest()).unwrap();
        retired["components"] = json!([]);
        let retired: ServingManifest = serde_json::from_value(retired).unwrap();
        let retired = LoadedRelease::load_canonical_bytes(
            &retired.canonical_bytes(),
            "retired-component",
            upgraded.scope().clone(),
        )
        .unwrap();
        let client = wamn_platform_identity::nats::PlatformNats::from_env()
            .unwrap()
            .connect()
            .await
            .unwrap();
        let broker = async_nats::jetstream::new(client.clone());
        let scope = Triple::new("schedule-live", PROJECT, ENVIRONMENT);
        let source = wamn_control_provision::events::source_stream_config(
            &scope,
            1,
            Duration::from_secs(120),
        );
        let advisory = wamn_control_provision::events::advisory_stream_config(&scope, 1);
        let stream = broker.create_stream(source).await.unwrap();
        let advisory = broker.create_stream(advisory).await.unwrap();
        let events = WamnJetstream::new(WamnJetstreamConfig {
            nats: Some(client),
            event_scope: Some(scope),
            stream_replicas: Some(1),
            dup_window_secs: Some(120),
        });
        let subject = format!("evt.schedule-live.{PROJECT}.{ENVIRONMENT}.order.insert");
        let mut expected = std::collections::BTreeMap::new();
        // B continues while A is stopped, upgraded, then rolled back. No control
        // record selects either release: only the hosts' loaded bytes change.
        for (lsn, loaded) in [
            (40, fixture.release.as_ref()),
            (41, &upgraded),
            (42, &upgraded),
            (43, fixture.release.as_ref()),
        ] {
            let body = envelope("insert", txid, lsn);
            let id = wamn_event_wire::msg_id(PROJECT, ENVIRONMENT, lsn);
            let mut headers = async_nats::HeaderMap::new();
            headers.insert(async_nats::header::NATS_MESSAGE_ID, id.as_str());
            let sequence = broker
                .publish_with_headers(subject.clone(), headers, body.clone().into())
                .await
                .unwrap()
                .await
                .unwrap()
                .sequence;
            let admission = EventBatchAdmission {
                source_package: APPLICATION,
                entity: "order",
                envelope: &body,
                sequence,
                source_event_id: &id,
            };
            events
                .route_event(&fixture.postgres, &admission)
                .await
                .unwrap();
            events
                .route_event(&fixture.postgres, &admission)
                .await
                .unwrap();
            events
                .admit_application_events(&fixture.postgres, &billing)
                .await
                .unwrap();
            let delivery = format!("event:{sequence}:{id}");
            expected.insert(
                ("billing".to_owned(), delivery.clone()),
                billing.release().manifest_digest.as_str().to_owned(),
            );
            let before = fixture
                .admin
                .query(
                    "SELECT package_id FROM wamn_run.runs WHERE invoked_by = $1",
                    &[&delivery],
                )
                .await
                .unwrap();
            assert_eq!(before.len(), 1, "B admits while A is stopped");
            assert_eq!(before[0].get::<_, String>(0), "billing");
            let mut expected_digest = loaded.release().manifest_digest.as_str().to_owned();
            if lsn == 40 {
                // Commit on A1, lose its ACK, then redeliver to a host without
                // that component. The original run must retain A1's digest.
                let consumers = stream.consumer_names().collect::<Vec<_>>().await;
                let mut pending = None;
                for name in consumers {
                    let consumer = stream
                        .get_consumer::<async_nats::jetstream::consumer::pull::Config>(
                            &name.unwrap(),
                        )
                        .await
                        .unwrap();
                    if consumer.cached_info().num_pending > 0 {
                        pending = Some(consumer);
                    }
                }
                let consumer = pending.unwrap();
                let mut messages = consumer.fetch().max_messages(1).messages().await.unwrap();
                let message = messages.next().await.unwrap().unwrap();
                assert!(
                    fixture
                        .postgres
                        .admit_event_batch(PROJECT, &retired, &admission)
                        .await
                        .is_err(),
                    "a host cannot admit a workflow absent from its loaded manifest"
                );
                fixture
                    .postgres
                    .admit_event_batch(PROJECT, loaded, &admission)
                    .await
                    .unwrap();
                message
                    .ack_with(async_nats::jetstream::AckKind::Nak(None))
                    .await
                    .unwrap();
                events
                    .admit_application_events(&fixture.postgres, &retired)
                    .await
                    .unwrap();
            } else if lsn == 41 {
                let (old, new) = tokio::join!(
                    fixture
                        .postgres
                        .admit_event_batch(PROJECT, &fixture.release, &admission),
                    fixture
                        .postgres
                        .admit_event_batch(PROJECT, &upgraded, &admission),
                );
                old.unwrap();
                new.unwrap();
                expected_digest = fixture.admin.query_one(
                    "SELECT manifest_digest FROM wamn_run.runs WHERE package_id = $1 AND invoked_by = $2",
                    &[&APP, &delivery],
                ).await.unwrap().get(0);
                assert!(
                    [
                        fixture.digest.as_str(),
                        upgraded.release().manifest_digest.as_str()
                    ]
                    .contains(&expected_digest.as_str())
                );
                events
                    .admit_application_events(&fixture.postgres, &upgraded)
                    .await
                    .unwrap();
            } else {
                events
                    .admit_application_events(&fixture.postgres, loaded)
                    .await
                    .unwrap();
            }
            expected.insert((APP.to_owned(), delivery), expected_digest);
            let rows = fixture
                .admin
                .query(
                    "SELECT package_id, invoked_by, manifest_digest FROM wamn_run.runs",
                    &[],
                )
                .await
                .unwrap();
            assert_eq!(
                rows.len(),
                expected.len(),
                "exactly one run per application and event"
            );
            let actual: std::collections::BTreeMap<_, _> = rows
                .iter()
                .map(|row| {
                    (
                        (row.get::<_, String>(0), row.get::<_, String>(1)),
                        row.get::<_, String>(2),
                    )
                })
                .collect();
            assert_eq!(actual, expected);
        }
        for name in stream.consumer_names().collect::<Vec<_>>().await {
            let consumer = stream
                .get_consumer::<async_nats::jetstream::consumer::pull::Config>(&name.unwrap())
                .await
                .unwrap();
            assert_eq!(consumer.cached_info().num_pending, 0);
            assert_eq!(consumer.cached_info().num_ack_pending, 0);
        }
        broker
            .delete_stream(&stream.cached_info().config.name)
            .await
            .unwrap();
        broker
            .delete_stream(&advisory.cached_info().config.name)
            .await
            .unwrap();
    }
}

#[tokio::test]
#[ignore = "requires: env:WAMN_NATS_URL, env:WAMN_NATS_TLS_CA, env:WAMN_NATS_TLS_CERT, env:WAMN_NATS_TLS_KEY"]
async fn reset_discards_only_the_named_environments_backlog() -> anyhow::Result<()> {
    use wamn_control_provision::events::{advisory_stream_config, source_stream_config};
    use wamn_environment_control::event_streams;
    wamn_test_postgres::require_ignored_prerequisites(
        "requires: env:WAMN_NATS_URL, env:WAMN_NATS_TLS_CA, env:WAMN_NATS_TLS_CERT, env:WAMN_NATS_TLS_KEY",
    );
    let client = wamn_platform_identity::nats::PlatformNats::from_env()?
        .connect()
        .await?;
    let broker = async_nats::jetstream::new(client);
    let project = format!("p{}", &uuid::Uuid::new_v4().simple().to_string()[..12]);
    let target = Triple::new("reset", &project, "dev");
    let other = Triple::new("reset", &project, "other");
    let mut streams = Vec::new();
    for scope in [&target, &other] {
        event_streams::provision(&broker, scope, 1, Duration::from_secs(120), &[]).await?;
        for config in [
            source_stream_config(scope, 1, Duration::from_secs(120)),
            advisory_stream_config(scope, 1),
        ] {
            let subject = config.subjects[0]
                .replace('>', "widget.insert")
                .replace('*', "message");
            broker.publish(subject, "old event".into()).await?.await?;
            streams.push((scope == &target, config));
        }
    }
    let result = async {
        event_streams::discard(&broker, &target).await?;
        for (target, config) in &streams {
            let mut stream = broker.get_stream(&config.name).await?;
            let actual = stream.info().await?;
            assert!(wamn_control_provision::events::stream_config_matches(
                config,
                &actual.config
            ));
            assert_eq!(actual.state.messages, u64::from(!target));
        }
        // A retry retains the declarations and leaves the other environment alone.
        event_streams::discard(&broker, &target).await?;
        Ok::<_, anyhow::Error>(())
    }
    .await;
    for (_, config) in streams {
        broker.delete_stream(&config.name).await?;
    }
    result
}
