//! Route environment events to application hosts through durable consumers.
#![expect(unsafe_code, reason = "wit-bindgen generated FFI glue")]
#[allow(
    clippy::same_length_and_capacity,
    reason = "wit-bindgen 0.61 emits Vec::from_raw_parts with equal length and capacity"
)]
mod bindings {
    wit_bindgen::generate!({
        world: "wamn:materializer/materializer@0.1.0",
        path: [
            "../../../../wit/wamn-flow-http-routing",
            "../../../../wit/wamn-jetstream",
            "../../../../wit/wamn-postgres-0.4",
            "../../../../wit/wamn-router-delivery-0.4",
            "wit",
        ],
        generate_all,
    });
}

use std::collections::BTreeMap;

use serde_json::Value;
use wamn_event_reg::{EventRegistration, RegistrationInput};
use wamn_event_wire::{DerivedEvent, Envelope};
use wamn_materializer::{verified_derived_source_event_id, verified_source_event_id};

#[cfg(not(test))]
use bindings::events::{self, MessageHandle as Message};
#[cfg(test)]
use bindings::wamn::jetstream::registration::ConsumerConfig;
#[cfg(not(test))]
use bindings::wamn::jetstream::registration::{self, ConsumerConfig};
use bindings::wasmcloud::nats::types::HeaderEntry as Header;
#[cfg(test)]
use tests::{events, events::MessageHandle as Message, registration};

struct Component;

impl bindings::exports::wasi::cli::run::Guest for Component {
    async fn run() -> Result<(), ()> {
        run_service().await;
        Ok(())
    }
}

bindings::export!(Component with_types_in bindings);

struct Config {
    stream: String,
    org: String,
    project: String,
    env: String,
    project_env_key: String,
    batch: u32,
    fetch_ms: u32,
    sweep_ms: u64,
    max_sweeps: u64,
    ack_wait_ms: u64,
    nack_delay_ms: u32,
    max_deliver: u32,
    report_path: Option<String>,
}

fn env_or(name: &str, default: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| default.to_string())
}

fn required(name: &str) -> Result<String, String> {
    std::env::var(name).map_err(|_| format!("missing required env {name}"))
}

/// The declared `max_batch` of `materializer_consumer_config`
/// (crates/control/provision/src/events.rs). A larger fetch is refused by the
/// server, so the only runtime cause of `limit-exceeded` stays the waiting pull.
const MAX_BATCH: u32 = 64;

fn batch(value: &str) -> Result<u32, String> {
    let batch = value
        .parse::<u32>()
        .map_err(|error| format!("WAMN_MAT_BATCH: {error}"))?;
    if batch > MAX_BATCH {
        return Err(format!("WAMN_MAT_BATCH must not exceed {MAX_BATCH}"));
    }
    Ok(batch)
}

impl Config {
    fn from_env() -> Result<Self, String> {
        let max_deliver = env_or("WAMN_MAT_MAX_DELIVER", "5")
            .parse::<u32>()
            .map_err(|error| format!("WAMN_MAT_MAX_DELIVER: {error}"))?;
        if max_deliver == 0 {
            return Err("WAMN_MAT_MAX_DELIVER must be non-zero".into());
        }
        Ok(Self {
            stream: required("WAMN_MAT_STREAM")?,
            org: required("WAMN_MAT_ORG")?,
            project: required("WAMN_MAT_PROJECT")?,
            env: required("WAMN_MAT_ENV")?,
            project_env_key: required("WAMN_MAT_PROJECT_ENV_KEY")?,
            batch: batch(&env_or("WAMN_MAT_BATCH", "64"))?,
            fetch_ms: env_or("WAMN_MAT_FETCH_MS", "5000")
                .parse()
                .map_err(|error| format!("WAMN_MAT_FETCH_MS: {error}"))?,
            sweep_ms: env_or("WAMN_MAT_SWEEP_MS", "10000")
                .parse()
                .map_err(|error| format!("WAMN_MAT_SWEEP_MS: {error}"))?,
            max_sweeps: env_or("WAMN_MAT_MAX_SWEEPS", "0")
                .parse()
                .map_err(|error| format!("WAMN_MAT_MAX_SWEEPS: {error}"))?,
            ack_wait_ms: env_or("WAMN_MAT_ACK_WAIT_MS", "30000")
                .parse()
                .map_err(|error| format!("WAMN_MAT_ACK_WAIT_MS: {error}"))?,
            nack_delay_ms: env_or("WAMN_MAT_NACK_DELAY_MS", "5000")
                .parse()
                .map_err(|error| format!("WAMN_MAT_NACK_DELAY_MS: {error}"))?,
            max_deliver,
            report_path: std::env::var("WAMN_MAT_REPORT_PATH").ok(),
        })
    }
}

#[derive(Default)]
struct Counters {
    sweeps: u64,
    deliveries: u64,
    batches: u64,
    acked: u64,
    poison: u64,
    retry: u64,
    terminated: u64,
    termination_retry: u64,
}

impl Counters {
    fn to_json(&self) -> String {
        serde_json::json!({
            "sweeps": self.sweeps,
            "deliveries": self.deliveries,
            "batches": self.batches,
            "acked": self.acked,
            "poison": self.poison,
            "retry": self.retry,
            "terminated": self.terminated,
            "termination-retry": self.termination_retry,
        })
        .to_string()
    }
}

struct Serving {
    registration: EventRegistration,
}

fn nats_message_ids(headers: &[Header]) -> Vec<&str> {
    headers
        .iter()
        .filter(|header| header.name.eq_ignore_ascii_case("Nats-Msg-Id"))
        .map(|header| header.value.as_str())
        .collect()
}

async fn load_servings() -> Result<Vec<Serving>, String> {
    let document = registration::triggers()
        .await
        .map_err(|error| format!("{error:?}"))?;
    let rows: Vec<wamn_materializer::workflow::EventTrigger> =
        serde_json::from_str(&document).map_err(|error| error.to_string())?;
    Ok(rows
        .into_iter()
        .map(|row| Serving {
            registration: EventRegistration {
                schema_version: wamn_event_reg::SCHEMA_VERSION.to_owned(),
                registration_id: row.workflow_id,
                package_id: row.application,
                source_package_id: row.source_package,
                entity: row.entity,
                ops: row.ops,
                condition: row.condition,
                input: RegistrationInput::Event,
            },
        })
        .collect())
}

enum Preparation {
    Deliver { source_event_id: String },
    Nack,
    Terminate(&'static str),
}

#[derive(Debug, PartialEq)]
enum SourceEvent {
    Cdc(Envelope),
    Derived(DerivedEvent),
}

fn decode_source_event(body: &[u8]) -> Result<SourceEvent, &'static str> {
    let value = serde_json::from_slice::<Value>(body).map_err(|_| "poison-invalid-envelope")?;
    if value.get("format-version").is_some() {
        DerivedEvent::from_slice(body)
            .map(SourceEvent::Derived)
            .map_err(|_| "poison-invalid-derived-event")
    } else {
        serde_json::from_value::<Envelope>(value)
            .map(SourceEvent::Cdc)
            .map_err(|_| "poison-invalid-envelope")
    }
}

fn prepare_message(config: &Config, message: &Message, counters: &mut Counters) -> Preparation {
    let stream_seq = message.sequence();
    if stream_seq == 0 {
        counters.retry += 1;
        eprintln!("wamn::materializer metadata parse failure — nack for redelivery");
        return Preparation::Nack;
    }
    let contents = message.message();
    let source = match decode_source_event(&contents.body) {
        Ok(source) => source,
        Err(reason) => {
            counters.poison += 1;
            eprintln!(
                "wamn::materializer REFUSED poison stream_seq={stream_seq}: event record parse failed"
            );
            return Preparation::Terminate(reason);
        }
    };
    let headers = contents.headers.unwrap_or_default();
    let message_ids = nats_message_ids(&headers);
    let source_event_id = match &source {
        SourceEvent::Cdc(envelope) => {
            verified_source_event_id(&config.project, &config.env, envelope, &message_ids)
        }
        SourceEvent::Derived(event) => verified_derived_source_event_id(
            &config.project_env_key,
            &config.project,
            &config.env,
            event,
            &message_ids,
        ),
    };
    if let Some(id) = source_event_id {
        Preparation::Deliver {
            source_event_id: id.as_str().to_owned(),
        }
    } else {
        counters.poison += 1;
        Preparation::Terminate("poison-source-id")
    }
}

async fn acknowledge(message: &Message, counters: &mut Counters) {
    match message.ack_sync().await {
        Ok(()) => counters.acked += 1,
        Err(error) => {
            counters.retry += 1;
            eprintln!("wamn::materializer ack failed ({error:?}); server redelivery remains armed");
        }
    }
}

async fn nack(message: &Message, config: &Config, counters: &mut Counters) {
    counters.retry += 1;
    if let Err(error) = message.nak(Some(config.nack_delay_ms)).await {
        eprintln!("wamn::materializer nack failed ({error:?}); ack-wait redelivery remains armed");
    }
}

async fn terminate(message: &Message, reason: &'static str, counters: &mut Counters) {
    match message.term().await {
        Ok(()) => counters.terminated += 1,
        Err(error) => {
            counters.termination_retry += 1;
            eprintln!(
                "wamn::materializer term failed reason={reason} ({error:?}); broker redelivery remains armed"
            );
        }
    }
}

async fn serve(config: &Config, serving: &Serving, counters: &mut Counters) {
    let registration = &serving.registration;
    let durable = wamn_materializer::workflow::entity_durable(
        &registration.source_package_id,
        &registration.entity,
    );
    let entity = wamn_event_wire::subject_token(registration.entity.as_str());
    let filter = wamn_registry::name::event_subject(
        &config.org,
        &config.project,
        &config.env,
        &wamn_registry::name::nats_subject(&[&entity, ">"]),
    );
    let provisioned = registration::prepare(
        registration.package_id.clone(),
        registration.registration_id.clone(),
        ConsumerConfig {
            stream_name: config.stream.clone(),
            durable: durable.clone(),
            filter_subject: filter,
            ack_wait_ms: config.ack_wait_ms,
            max_deliver: config.max_deliver,
        },
    )
    .await;
    if let Err(error) = provisioned {
        eprintln!(
            "wamn::materializer registration preparation failed for {}: {error:?}",
            registration.registration_id
        );
        return;
    }
    let consumer = match events::open_pull_consumer(config.stream.clone(), durable).await {
        Ok(consumer) => consumer,
        Err(error) => {
            eprintln!(
                "wamn::materializer native attachment failed for {}: {error:?}",
                registration.registration_id
            );
            return;
        }
    };
    let messages = match consumer.fetch(config.batch, config.fetch_ms).await {
        Ok(batch) => batch.messages,
        // `limit-exceeded`: another replica holds the consumer's one waiting pull
        // (`max_waiting: 1`, crates/control/provision/src/events.rs). Every
        // replica but one is in this state.
        Err(
            bindings::wasmcloud::nats::types::NatsError::NoMessages
            | bindings::wasmcloud::nats::types::NatsError::LimitExceeded(_),
        ) => return,
        Err(error) => {
            eprintln!(
                "wamn::materializer native fetch failed for {}: {error:?}",
                registration.registration_id
            );
            return;
        }
    };
    for message in messages {
        let source_event_id = match prepare_message(config, &message, counters) {
            Preparation::Deliver { source_event_id } => source_event_id,
            Preparation::Nack => {
                nack(&message, config, counters).await;
                continue;
            }
            Preparation::Terminate(reason) => {
                terminate(&message, reason, counters).await;
                continue;
            }
        };
        let contents = message.message();
        let Ok(envelope) = String::from_utf8(contents.body) else {
            terminate(&message, "poison-invalid-envelope", counters).await;
            continue;
        };
        match registration::admit(
            registration.source_package_id.clone(),
            registration.entity.clone(),
            envelope,
            message.sequence(),
            source_event_id,
        )
        .await
        {
            Ok(()) => {
                counters.deliveries += 1;
                acknowledge(&message, counters).await;
            }
            Err(error) => {
                eprintln!("wamn::materializer routing failed: {error:?}");
                nack(&message, config, counters).await;
            }
        }
    }
}

fn write_report(config: &Config, counters: &Counters) {
    if let Some(path) = &config.report_path
        && let Err(error) = std::fs::write(path, counters.to_json())
    {
        eprintln!("wamn::materializer report write failed ({path}): {error}");
    }
}

async fn run_service() {
    let config = match Config::from_env() {
        Ok(config) => config,
        Err(error) => {
            eprintln!("wamn::materializer config error: {error}");
            std::process::exit(1);
        }
    };
    println!(
        "wamn::materializer up: stream={} filter={} project_env_key={} batch={} fetch_ms={} max_deliver={} max_sweeps={}",
        config.stream,
        wamn_registry::name::event_subject(&config.org, &config.project, &config.env, "*.*"),
        config.project_env_key,
        config.batch,
        config.fetch_ms,
        config.max_deliver,
        config.max_sweeps
    );
    let mut counters = Counters::default();
    loop {
        counters.sweeps += 1;
        match load_servings().await {
            Ok(loaded) if loaded.is_empty() => {
                bindings::wasi::clocks::monotonic_clock::wait_for(
                    config.sweep_ms.saturating_mul(1_000_000),
                )
                .await;
            }
            Ok(loaded) => {
                let mut entities = BTreeMap::new();
                for serving in &loaded {
                    entities
                        .entry((
                            &serving.registration.source_package_id,
                            &serving.registration.entity,
                        ))
                        .or_insert(serving);
                }
                for serving in entities.values() {
                    serve(&config, serving, &mut counters).await;
                }
            }
            Err(error) => {
                eprintln!(
                    "wamn::materializer sweep failed ({error}); retrying after {}ms",
                    config.sweep_ms
                );
                bindings::wasi::clocks::monotonic_clock::wait_for(
                    config.sweep_ms.saturating_mul(1_000_000),
                )
                .await;
            }
        }
        write_report(&config, &counters);
        if config.max_sweeps > 0 && counters.sweeps >= config.max_sweeps {
            println!(
                "wamn::materializer done after {} sweeps: {}",
                counters.sweeps,
                counters.to_json()
            );
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wamn_event_wire::{Causation, Op};

    #[expect(
        clippy::unused_async,
        clippy::unused_async_trait_impl,
        reason = "test methods match the asynchronous WIT imports"
    )]
    pub(super) mod events {
        use crate::bindings::wasmcloud::nats::types::{NatsError, NatsMessage};
        use std::cell::RefCell;
        use std::rc::Rc;

        std::thread_local! {
            pub static MESSAGES: RefCell<Vec<MessageHandle>> = const { RefCell::new(Vec::new()) };
        }

        #[derive(Debug, PartialEq)]
        pub enum Settlement {
            Ack,
            Nack(Option<u32>),
            Terminate,
        }

        #[derive(Clone)]
        pub struct MessageHandle {
            pub contents: NatsMessage,
            pub sequence: u64,
            pub settlements: Rc<RefCell<Vec<Settlement>>>,
        }

        impl MessageHandle {
            pub fn message(&self) -> NatsMessage {
                self.contents.clone()
            }

            pub fn sequence(&self) -> u64 {
                self.sequence
            }

            pub async fn ack_sync(&self) -> Result<(), NatsError> {
                self.settlements.borrow_mut().push(Settlement::Ack);
                Ok(())
            }

            pub async fn nak(&self, delay: Option<u32>) -> Result<(), NatsError> {
                self.settlements.borrow_mut().push(Settlement::Nack(delay));
                Ok(())
            }

            pub async fn term(&self) -> Result<(), NatsError> {
                self.settlements.borrow_mut().push(Settlement::Terminate);
                Ok(())
            }
        }

        pub struct Consumer;

        pub struct Batch {
            pub messages: Vec<MessageHandle>,
        }

        pub async fn open_pull_consumer(_: String, _: String) -> Result<Consumer, NatsError> {
            Ok(Consumer)
        }

        impl Consumer {
            pub async fn fetch(&self, _: u32, _: u32) -> Result<Batch, NatsError> {
                Ok(Batch {
                    messages: MESSAGES.take(),
                })
            }
        }
    }

    #[expect(
        clippy::unused_async,
        reason = "test functions match the asynchronous WIT imports"
    )]
    pub(super) mod registration {
        use crate::bindings::wamn::jetstream::registration::ConsumerConfig;
        pub use crate::bindings::wamn::jetstream::registration::triggers;
        use crate::bindings::wamn::jetstream::types::JsError;
        use std::cell::Cell;

        std::thread_local! {
            pub static TRANSIENT: Cell<bool> = const { Cell::new(false) };
            pub static ADMITS: Cell<usize> = const { Cell::new(0) };
        }

        pub async fn prepare(_: String, _: String, _: ConsumerConfig) -> Result<(), JsError> {
            Ok(())
        }

        pub async fn admit(
            _: String,
            _: String,
            _: String,
            _: u64,
            _: String,
        ) -> Result<(), JsError> {
            ADMITS.set(ADMITS.get() + 1);
            if TRANSIENT.get() {
                Err(JsError::ConnectionUnavailable)
            } else {
                Ok(())
            }
        }
    }

    fn ready<T>(future: impl std::future::Future<Output = T>) -> T {
        let mut future = std::pin::pin!(future);
        let mut context = std::task::Context::from_waker(std::task::Waker::noop());
        match future.as_mut().poll(&mut context) {
            std::task::Poll::Ready(value) => value,
            std::task::Poll::Pending => panic!("the test host must complete immediately"),
        }
    }

    fn config() -> Config {
        Config {
            stream: "events".into(),
            org: "org".into(),
            project: "app".into(),
            env: "dev".into(),
            project_env_key: "project".into(),
            batch: 1,
            fetch_ms: 1,
            sweep_ms: 1,
            max_sweeps: 1,
            ack_wait_ms: 30000,
            nack_delay_ms: 1234,
            max_deliver: 5,
            report_path: None,
        }
    }

    fn message() -> Message {
        Message {
            contents: bindings::wasmcloud::nats::types::NatsMessage {
                subject: wamn_registry::name::event_subject("org", "app", "dev", "receipts.insert"),
                body: serde_json::to_vec(&envelope()).unwrap(),
                reply_to: None,
                headers: Some(vec![Header {
                    name: "Nats-Msg-Id".into(),
                    value: "app_dev:42".into(),
                }]),
            },
            sequence: 42,
            settlements: std::rc::Rc::default(),
        }
    }

    fn serve_message(serving: &Serving, message: Message) -> Counters {
        events::MESSAGES.set(vec![message]);
        registration::ADMITS.set(0);
        let mut counters = Counters::default();
        ready(serve(&config(), serving, &mut counters));
        counters
    }

    #[test]
    fn refused_message_terminates_without_admission() {
        let serving = serving();
        let message = message();
        let mut message = message;
        message.contents.headers = None;
        assert!(matches!(
            prepare_message(&config(), &message, &mut Counters::default()),
            Preparation::Terminate("poison-source-id")
        ));
        let counters = serve_message(&serving, message.clone());
        assert_eq!(
            *message.settlements.borrow(),
            vec![events::Settlement::Terminate]
        );
        assert_eq!(registration::ADMITS.get(), 0);
        assert_eq!(counters.terminated, 1);
    }

    #[test]
    fn transient_admission_failure_nacks_with_delay() {
        registration::TRANSIENT.set(true);
        let message = message();
        let counters = serve_message(&serving(), message.clone());
        assert_eq!(
            *message.settlements.borrow(),
            vec![events::Settlement::Nack(Some(1234))]
        );
        assert_eq!(registration::ADMITS.get(), 1);
        assert_eq!(counters.retry, 1);
        assert_eq!(counters.acked, 0);
        registration::TRANSIENT.set(false);
    }

    #[test]
    fn routing_does_not_evaluate_application_conditions() {
        let mut serving = serving();
        serving.registration.condition = Some("invalid condition".into());
        let message = message();
        let counters = serve_message(&serving, message.clone());
        assert_eq!(*message.settlements.borrow(), vec![events::Settlement::Ack]);
        assert_eq!(registration::ADMITS.get(), 1);
        assert_eq!(counters.acked, 1);
    }

    #[test]
    fn a_batch_above_the_declared_max_batch_is_refused() {
        assert_eq!(batch("64"), Ok(64));
        assert_eq!(
            batch("65"),
            Err("WAMN_MAT_BATCH must not exceed 64".to_string())
        );
    }

    fn serving() -> Serving {
        Serving {
            registration: EventRegistration {
                schema_version: wamn_event_reg::SCHEMA_VERSION.into(),
                registration_id: "receive-receipt".into(),
                package_id: "client_acme_receiving".into(),
                source_package_id: "receiving".into(),
                entity: "receipts".into(),
                ops: vec![Op::Insert],
                input: RegistrationInput::Event,
                condition: None,
            },
        }
    }

    fn envelope() -> Envelope {
        serde_json::from_value(serde_json::json!({
            "op": "insert",
            "new": {"project_id": "project"},
            "package_id": "receiving",
            "entity": "receipts",
            "table": "receipts",
            "lsn": 42,
            "txid": 7,
            "commit_ts": "2026-08-15T12:00:00Z"
        }))
        .unwrap()
    }

    #[test]
    fn source_event_id_requires_one_exact_header() {
        let matching = vec![Header {
            name: "nats-msg-id".into(),
            value: "app_dev:42".into(),
        }];
        let ids = nats_message_ids(&matching);
        assert_eq!(
            verified_source_event_id("app", "dev", &envelope(), &ids)
                .unwrap()
                .as_str(),
            "app_dev:42"
        );
        assert!(
            verified_source_event_id("app", "dev", &envelope(), &[]).is_none(),
            "a missing source id must remain poison"
        );
    }

    #[test]
    fn cdc_decode_refuses_either_half_of_package_entity_identity() {
        let complete = serde_json::json!({
            "op": "insert",
            "new": {"project_id": "project"},
            "package_id": "receiving",
            "entity": "receipts",
            "table": "receipts",
            "lsn": 42,
            "txid": 7,
            "commit_ts": "2026-08-15T12:00:00Z"
        });
        for missing in ["package_id", "entity"] {
            let mut half = complete.clone();
            half.as_object_mut().unwrap().remove(missing);
            assert_eq!(
                decode_source_event(&serde_json::to_vec(&half).unwrap()),
                Err("poison-invalid-envelope"),
                "missing {missing} must refuse at the wire boundary"
            );
        }
    }

    #[test]
    fn derived_origin_decodes_separately_from_the_cdc_envelope() {
        let event = DerivedEvent::new(
            "project",
            "app",
            "dev",
            "receiving",
            "receipts",
            Op::Delete,
            serde_json::json!(["arbitrary", {"id": 7}]),
            "author:receipt:7",
            Causation {
                run: "delivery-7".into(),
                root: "delivery-1".into(),
                depth: 2,
            },
        );
        let bytes = serde_json::to_vec(&event).unwrap();
        assert_eq!(
            decode_source_event(&bytes),
            Ok(SourceEvent::Derived(event.clone()))
        );
        assert!(serde_json::from_slice::<Envelope>(&bytes).is_err());

        let complete = serde_json::to_value(&event).unwrap();
        for missing in ["package-id", "entity"] {
            let mut incomplete = complete.clone();
            incomplete.as_object_mut().unwrap().remove(missing);
            assert_eq!(
                decode_source_event(&serde_json::to_vec(&incomplete).unwrap()),
                Err("poison-invalid-derived-event"),
                "missing {missing} must typed-refuse at the derived-event boundary"
            );
        }

        let mut future = serde_json::to_value(event).unwrap();
        future["format-version"] = "0.2".into();
        assert_eq!(
            decode_source_event(&serde_json::to_vec(&future).unwrap()),
            Err("poison-invalid-derived-event")
        );
    }
}
