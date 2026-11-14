//! Application admission during upgrade, rollback, and broker redelivery.
#[path = "support/schedule_fixture.rs"]
pub mod support;
use support::*;

use sqlx::Row as _;
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
        let mut fixture = fixture().await;
        fixture.grant_workflow_user("reader").await;
        let txid = changed_order(&mut fixture).await;
        event(&mut fixture, "first", 0, Some("reader")).await;
        sqlx::query("INSERT INTO catalog.workflows (project_env_key, environment, application, workflow_id, version, graph_json, wiring_hash, trigger, source_package, entity, ops, enable_sequence, execution_type, execution_role, enabled, created_by) \
             SELECT project_env_key, environment, 'billing', workflow_id, version, graph_json, wiring_hash, trigger, source_package, entity, ops, enable_sequence, execution_type, execution_role, enabled, created_by FROM catalog.workflows WHERE application = $1").bind(&APP).execute(&mut fixture.admin).await.map(|result| result.rows_affected()).unwrap();
        let upgraded = release_variant(&mut fixture, APP, b"1251").await;
        let billing = release_variant(&mut fixture, "billing", b"1252").await;
        let mut retired = serde_json::to_value(upgraded.manifest()).unwrap();
        retired["components"] = json!([]);
        let retired: ServingManifest = serde_json::from_value(retired).unwrap();
        let retired = LoadedRelease::load_canonical_bytes(
            &retired.canonical_bytes(),
            "retired-component",
            upgraded.target().clone(),
        )
        .unwrap();
        let client = wamn_broker::nats::PlatformNats::from_env()
            .unwrap()
            .connect()
            .await
            .unwrap();
        let broker = async_nats::jetstream::new(client.clone());
        let coordinate = Triple::new("schedule-live", ENVIRONMENT, PROJECT);
        let source =
            wamn_broker::events::source_stream_config(&coordinate, 1, Duration::from_secs(120));
        let advisory = wamn_broker::events::advisory_stream_config(&coordinate, 1);
        let stream = broker.create_stream(source).await.unwrap();
        let advisory = broker.create_stream(advisory).await.unwrap();
        let consumers = [APP, "billing"].map(|application| {
            wamn_broker::events::application_consumer(
                &coordinate,
                application,
                APPLICATION,
                "order",
            )
            .unwrap()
        });
        wamn_broker::streams::prepare_application_consumers(
            &wamn_broker::nats::PlatformNats::from_env().unwrap(),
            &coordinate,
            &consumers,
        )
        .await
        .unwrap();
        let events = WamnJetstream::new(WamnJetstreamConfig {
            nats: Some(client),
            event_target: Some(coordinate),
            stream_replicas: Some(1),
            dup_window_secs: Some(120),
        })
        .await
        .unwrap();
        let subject = wamn_registry::name::event_subject(
            "schedule-live",
            PROJECT,
            ENVIRONMENT,
            "order.insert",
        );
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
            let id = wamn_event_wire::msg_id(
                PROJECT,
                ENVIRONMENT,
                &wamn_event_wire::CdcIdentity {
                    system_id: 1,
                    timeline: 1,
                    commit_lsn: lsn,
                    row_ordinal: 0,
                },
            );
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
            wamn_workflow::event_admission::admit_application_events(
                &events,
                &fixture.postgres,
                PROJECT,
                &billing,
            )
            .await
            .unwrap();
            let delivery = format!("event:{id}");
            expected.insert(
                ("billing".to_owned(), delivery.clone()),
                billing.release().manifest_digest.as_str().to_owned(),
            );
            let before = sqlx::query("SELECT package_id FROM wamn_run.runs WHERE invoked_by = $1")
                .bind(&delivery)
                .fetch_all(&mut fixture.admin)
                .await
                .unwrap();
            assert_eq!(before.len(), 1, "B admits while A is stopped");
            assert_eq!(before[0].get::<String, _>(0), "billing");
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
                    wamn_workflow::event_admission::admit_event_batch(
                        &fixture.postgres,
                        PROJECT,
                        &retired,
                        &admission
                    )
                    .await
                    .is_err(),
                    "a host cannot admit a workflow absent from its loaded manifest"
                );
                wamn_workflow::event_admission::admit_event_batch(
                    &fixture.postgres,
                    PROJECT,
                    loaded,
                    &admission,
                )
                .await
                .unwrap();
                message
                    .ack_with(async_nats::jetstream::AckKind::Nak(None))
                    .await
                    .unwrap();
                wamn_workflow::event_admission::admit_application_events(
                    &events,
                    &fixture.postgres,
                    PROJECT,
                    &retired,
                )
                .await
                .unwrap();
            } else if lsn == 41 {
                let (old, new) = tokio::join!(
                    wamn_workflow::event_admission::admit_event_batch(
                        &fixture.postgres,
                        PROJECT,
                        &fixture.release,
                        &admission
                    ),
                    wamn_workflow::event_admission::admit_event_batch(
                        &fixture.postgres,
                        PROJECT,
                        &upgraded,
                        &admission
                    ),
                );
                old.unwrap();
                new.unwrap();
                expected_digest = sqlx::query("SELECT manifest_digest FROM wamn_run.runs WHERE package_id = $1 AND invoked_by = $2").bind(&APP).bind(&delivery).fetch_one(&mut fixture.admin).await.unwrap().get(0);
                assert!(
                    [
                        fixture.digest.as_str(),
                        upgraded.release().manifest_digest.as_str()
                    ]
                    .contains(&expected_digest.as_str())
                );
                wamn_workflow::event_admission::admit_application_events(
                    &events,
                    &fixture.postgres,
                    PROJECT,
                    &upgraded,
                )
                .await
                .unwrap();
            } else {
                wamn_workflow::event_admission::admit_application_events(
                    &events,
                    &fixture.postgres,
                    PROJECT,
                    loaded,
                )
                .await
                .unwrap();
            }
            // A later stream sequence still names the original admitted event.
            // A host with another release must retain its run.
            let replay = EventBatchAdmission {
                sequence: sequence + 1000,
                ..admission
            };
            wamn_workflow::event_admission::admit_event_batch(
                &fixture.postgres,
                PROJECT,
                &upgraded,
                &replay,
            )
            .await
            .unwrap();
            expected.insert((APP.to_owned(), delivery), expected_digest);
            let rows =
                sqlx::query("SELECT package_id, invoked_by, manifest_digest FROM wamn_run.runs")
                    .fetch_all(&mut fixture.admin)
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
                        (row.get::<String, _>(0), row.get::<String, _>(1)),
                        row.get::<String, _>(2),
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
    use wamn_broker::events::{advisory_stream_config, source_stream_config};
    wamn_test_postgres::require_ignored_prerequisites(
        "requires: env:WAMN_NATS_URL, env:WAMN_NATS_TLS_CA, env:WAMN_NATS_TLS_CERT, env:WAMN_NATS_TLS_KEY",
    );
    let client = wamn_broker::nats::PlatformNats::from_env()?
        .connect()
        .await
        .unwrap();
    let broker = async_nats::jetstream::new(client);
    let project = format!("p{}", &uuid::Uuid::new_v4().simple().to_string()[..12]);
    let target = Triple::new("reset", "dev", &project);
    let other = Triple::new("reset", "other", &project);
    let mut streams = Vec::new();
    for coordinate in [&target, &other] {
        wamn_broker::streams::provision(&broker, coordinate, 1, Duration::from_secs(120), &[])
            .await
            .unwrap();
        for config in [
            source_stream_config(coordinate, 1, Duration::from_secs(120)),
            advisory_stream_config(coordinate, 1),
        ] {
            let subject = config.subjects[0]
                .replace('>', "widget.insert")
                .replace('*', "message");
            broker
                .publish(subject, "old event".into())
                .await?
                .await
                .unwrap();
            streams.push((coordinate == &target, config));
        }
    }
    let result = async {
        wamn_broker::streams::discard(&broker, &target)
            .await
            .unwrap();
        for (target, config) in &streams {
            let mut stream = broker.get_stream(&config.name).await.unwrap();
            let actual = stream.info().await.unwrap();
            assert!(wamn_broker::events::stream_config_matches(
                config,
                &actual.config
            ));
            assert_eq!(actual.state.messages, u64::from(!target));
        }
        // A retry retains the declarations and leaves the other environment alone.
        wamn_broker::streams::discard(&broker, &target)
            .await
            .unwrap();
        Ok::<_, anyhow::Error>(())
    }
    .await;
    for (_, config) in streams {
        broker.delete_stream(&config.name).await.unwrap();
    }
    result
}
