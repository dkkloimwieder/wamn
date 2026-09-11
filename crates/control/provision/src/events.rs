//! Native broker declarations for one event environment.

use std::time::Duration;

use async_nats::jetstream::consumer::{self, AckPolicy, IntoConsumerConfig, pull};
use async_nats::jetstream::stream::{self, Compression, RetentionPolicy, StorageType};
use wamn_control_registry::Triple;
use wamn_event_wire::{
    DELIVERY_ADVISORY_MAX_AGE_SECONDS, DELIVERY_ADVISORY_MAX_MESSAGES_PER_SUBJECT,
    delivery_advisory_stream, delivery_advisory_subjects, stream_subjects,
};

/// One materializer batch holds at most four MiB.
pub const MATERIALIZER_MAX_PULL_BYTES: i64 = 4 * 1024 * 1024;

/// Declare the source stream with explicit broker defaults.
pub fn source_stream_config(
    scope: &Triple,
    replicas: usize,
    duplicate_window: Duration,
) -> stream::Config {
    stream::Config {
        name: crate::event_stream_name(&scope.org, &scope.project, scope.env.as_str()),
        subjects: vec![stream_subjects(
            &scope.org,
            &scope.project,
            scope.env.as_str(),
        )],
        storage: StorageType::File,
        num_replicas: replicas,
        retention: RetentionPolicy::Limits,
        duplicate_window,
        max_consumers: -1,
        max_messages: -1,
        max_bytes: -1,
        max_messages_per_subject: -1,
        max_message_size: -1,
        compression: Some(Compression::None),
        ..Default::default()
    }
}

/// Declare bounded broker advisories for the same source stream.
pub fn advisory_stream_config(scope: &Triple, replicas: usize) -> stream::Config {
    let age = Duration::from_secs(DELIVERY_ADVISORY_MAX_AGE_SECONDS);
    let source = source_stream_config(scope, replicas, age);
    stream::Config {
        name: delivery_advisory_stream(&source.name),
        subjects: delivery_advisory_subjects(&source.name).to_vec(),
        max_age: age,
        max_messages_per_subject: DELIVERY_ADVISORY_MAX_MESSAGES_PER_SUBJECT,
        ..source
    }
}

/// Declare the existing bounded materializer delivery policy.
pub fn materializer_consumer_config(
    durable: &str,
    filter_subject: &str,
    ack_wait: Duration,
    max_deliver: u32,
) -> pull::Config {
    pull::Config {
        name: Some(durable.into()),
        durable_name: Some(durable.into()),
        ack_policy: AckPolicy::Explicit,
        filter_subject: filter_subject.into(),
        ack_wait,
        max_deliver: i64::from(max_deliver),
        max_ack_pending: 64,
        max_batch: 64,
        max_bytes: MATERIALIZER_MAX_PULL_BYTES,
        max_waiting: 1,
        ..Default::default()
    }
}

/// Compare every field in the declared and observed upstream stream types.
pub fn stream_config_matches(expected: &stream::Config, actual: &stream::Config) -> bool {
    expected == actual
}

/// Compare every field, including push delivery and additional subject filters.
pub fn consumer_config_matches(expected: &pull::Config, actual: &consumer::Config) -> bool {
    expected.into_consumer_config() == *actual
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_nats::jetstream::consumer::DeliverPolicy;
    use async_nats::jetstream::stream::{DiscardPolicy, Republish, Source, SubjectTransform};

    #[test]
    fn event_names_and_subjects_use_the_same_declared_triple() {
        let scope = Triple::new("acme", "receiving", "dev");
        let source = source_stream_config(&scope, 3, Duration::from_secs(120));
        assert_eq!(
            source.name,
            wamn_event_wire::stream_name("acme", "receiving", "dev")
        );
        assert_eq!(source.subjects, ["evt.acme.receiving.dev.>"]);
        let advisory = advisory_stream_config(&scope, 3);
        assert_eq!(advisory.name, delivery_advisory_stream(&source.name));
        assert_eq!(advisory.subjects, delivery_advisory_subjects(&source.name));
        assert_eq!(
            advisory.max_age,
            Duration::from_secs(DELIVERY_ADVISORY_MAX_AGE_SECONDS)
        );
        assert_eq!(
            advisory.max_messages_per_subject,
            DELIVERY_ADVISORY_MAX_MESSAGES_PER_SUBJECT
        );
        for other in [
            Triple::new("acme", "wms", "dev"),
            Triple::new("acme", "receiving", "prod"),
        ] {
            assert!(!stream_config_matches(
                &source,
                &source_stream_config(&other, 3, Duration::from_secs(120))
            ));
            assert!(!stream_config_matches(
                &advisory,
                &advisory_stream_config(&other, 3)
            ));
        }
    }

    #[test]
    fn source_comparison_refuses_foreign_inputs_and_changed_delivery_policy() {
        let expected = source_stream_config(
            &Triple::new("acme", "receiving", "dev"),
            3,
            Duration::from_secs(120),
        );
        assert!(stream_config_matches(&expected, &expected.clone()));
        let foreign = Source {
            name: "foreign".into(),
            ..Default::default()
        };
        let changed = [
            stream::Config {
                sources: Some(vec![foreign.clone()]),
                ..expected.clone()
            },
            stream::Config {
                mirror: Some(foreign),
                ..expected.clone()
            },
            stream::Config {
                subjects: vec!["evt.acme.*.dev.>".into()],
                ..expected.clone()
            },
            stream::Config {
                subject_transform: Some(SubjectTransform {
                    source: ">".into(),
                    destination: "foreign.>".into(),
                }),
                ..expected.clone()
            },
            stream::Config {
                republish: Some(Republish {
                    source: ">".into(),
                    destination: "foreign.>".into(),
                    headers_only: false,
                }),
                ..expected.clone()
            },
            stream::Config {
                retention: RetentionPolicy::WorkQueue,
                ..expected.clone()
            },
            stream::Config {
                discard: DiscardPolicy::New,
                ..expected.clone()
            },
            stream::Config {
                no_ack: true,
                ..expected.clone()
            },
            stream::Config {
                max_messages: 1,
                ..expected.clone()
            },
            stream::Config {
                allow_rollup: true,
                ..expected.clone()
            },
            stream::Config {
                allow_direct: true,
                ..expected.clone()
            },
            stream::Config {
                duplicate_window: Duration::from_secs(1),
                ..expected.clone()
            },
            stream::Config {
                num_replicas: 1,
                ..expected.clone()
            },
        ];
        for actual in changed {
            assert!(!stream_config_matches(&expected, &actual), "{actual:?}");
        }
    }

    #[test]
    fn consumer_comparison_refuses_foreign_delivery_and_changed_bounds() {
        let expected = materializer_consumer_config(
            "mat_t_pkg_r1",
            "evt.acme.receiving.dev.receipt.>",
            Duration::from_secs(30),
            5,
        );
        let stored = expected.clone().into_consumer_config();
        assert!(consumer_config_matches(&expected, &stored));
        let changed = [
            consumer::Config {
                durable_name: Some("foreign".into()),
                ..stored.clone()
            },
            consumer::Config {
                deliver_subject: Some("foreign.inbox".into()),
                ..stored.clone()
            },
            consumer::Config {
                deliver_group: Some("foreign".into()),
                ..stored.clone()
            },
            consumer::Config {
                filter_subjects: vec!["evt.acme.wms.dev.>".into()],
                ..stored.clone()
            },
            consumer::Config {
                ack_policy: AckPolicy::None,
                ..stored.clone()
            },
            consumer::Config {
                deliver_policy: DeliverPolicy::New,
                ..stored.clone()
            },
            consumer::Config {
                max_deliver: -1,
                ..stored.clone()
            },
            consumer::Config {
                max_ack_pending: 1000,
                ..stored.clone()
            },
            consumer::Config {
                max_waiting: 512,
                ..stored.clone()
            },
            consumer::Config {
                max_batch: 0,
                ..stored.clone()
            },
            consumer::Config {
                max_bytes: 0,
                ..stored.clone()
            },
            consumer::Config {
                headers_only: true,
                ..stored.clone()
            },
            consumer::Config {
                backoff: vec![Duration::from_secs(1)],
                ..stored.clone()
            },
        ];
        for actual in changed {
            assert!(!consumer_config_matches(&expected, &actual), "{actual:?}");
        }
    }
}
