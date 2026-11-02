//! Event fanout and acknowledgement through a real NATS broker.
#[path = "support/schedule_fixture.rs"]
pub mod support;
use support::*;

mod event_batch {
    use super::*;
    /// One durable consumes the entity message, even when two rows match it.
    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "requires: env:WAMN_NATS_URL, env:WAMN_NATS_TLS_CA, env:WAMN_NATS_TLS_CERT, env:WAMN_NATS_TLS_KEY"]
    async fn one_entity_message_fans_out_once_and_acknowledges_once() {
        use futures_util::StreamExt as _;
        wamn_test_postgres::require_ignored_prerequisites(
            "requires: env:WAMN_NATS_URL, env:WAMN_NATS_TLS_CA, env:WAMN_NATS_TLS_CERT, env:WAMN_NATS_TLS_KEY",
        );
        let _lock = wamn_test_postgres::lock();
        let fixture = fixture().await;
        fixture.grant_workflow_user("reader").await;
        let txid = changed_order(&fixture).await;
        assert!(fixture.admin.query_one(
        "SELECT to_regclass('catalog.wirings') IS NULL AND to_regclass('catalog.event_registrations') IS NULL", &[],
    ).await.unwrap().get::<_, bool>(0));
        assert!(
            serde_json::to_value(fixture.release.manifest())
                .unwrap()
                .get("workflow")
                .is_none()
        );
        event(&fixture, "first", 0, Some("reader")).await;
        event(&fixture, "second", 0, Some("reader")).await;
        let broker = async_nats::jetstream::new(
            wamn_platform_identity::nats::PlatformNats::from_env()
                .unwrap()
                .connect()
                .await
                .unwrap(),
        );
        let name = format!(
            "fanout_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let subject = format!("fixture.{name}.order.insert");
        let stream = broker
            .create_stream(async_nats::jetstream::stream::Config {
                name: name.clone(),
                subjects: vec![subject.clone()],
                ..Default::default()
            })
            .await
            .unwrap();
        let durable = "entity_6_orders_order";
        let mut consumer = stream
            .create_consumer(
                wamn_control_provision::events::materializer_consumer_config(
                    durable,
                    &subject,
                    Duration::from_secs(30),
                    5,
                ),
            )
            .await
            .unwrap();
        let body = envelope("insert", txid, 40);
        let published = broker
            .publish(subject, body.clone().into())
            .await
            .unwrap()
            .await
            .unwrap();
        let mut messages = consumer.fetch().max_messages(1).messages().await.unwrap();
        let first = messages.next().await.unwrap().unwrap();
        let sequence = first.info().unwrap().stream_sequence;
        assert_eq!(sequence, published.sequence);
        admit(&fixture, &body, sequence, 40).await.unwrap();
        let count = async || {
            fixture
                .admin
                .query_one("SELECT count(*) FROM wamn_run.runs", &[])
                .await
                .unwrap()
                .get::<_, i64>(0)
        };
        assert_eq!(count().await, 2);
        // A crash after commit but before ACK causes one redelivery.
        first
            .ack_with(async_nats::jetstream::AckKind::Nak(None))
            .await
            .unwrap();
        event(
            &fixture,
            "late",
            i64::try_from(sequence).unwrap(),
            Some("reader"),
        )
        .await;
        let mut messages = consumer.fetch().max_messages(1).messages().await.unwrap();
        let replay = messages.next().await.unwrap().unwrap();
        assert_eq!(replay.info().unwrap().stream_sequence, sequence);
        assert_eq!(replay.info().unwrap().delivered, 2);
        admit(&fixture, &body, sequence, 40).await.unwrap();
        assert_eq!(
            count().await,
            2,
            "redelivery and the late trigger admit no run"
        );
        replay.double_ack().await.unwrap();
        let info = consumer.info().await.unwrap();
        assert_eq!(info.num_ack_pending, 0);
        assert_eq!(info.num_pending, 0);
        assert_eq!(info.ack_floor.stream_sequence, sequence);
        assert_eq!(stream.consumer_names().collect::<Vec<_>>().await.len(), 1);
        broker.delete_stream(name).await.unwrap();
    }
}
