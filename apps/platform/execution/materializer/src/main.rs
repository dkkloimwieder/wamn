//! Durable JetStream registration delivery through the shared router bridge.

#[allow(
    clippy::same_length_and_capacity,
    reason = "wit-bindgen 0.61 emits Vec::from_raw_parts with equal length and capacity"
)]
mod bindings {
    wit_bindgen::generate!({
        world: "wamn:materializer/materializer@0.1.0",
        path: [
            "../../../../crates/platform/runtime/wit/deps/wamn-flow-http-routing",
            "../../../../crates/platform/runtime/wit/deps/wamn-jetstream",
            "../../../../crates/platform/runtime/wit/deps/wamn-postgres-0.4",
            "../../../../crates/execution/host/wit/deps/wamn-router-delivery-0.4",
            "wit",
        ],
        generate_all,
    });
}

use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;
use wamn_event_reg::{EventRegistration, RegistrationInput};
use wamn_event_wire::{DerivedEvent, Envelope};
use wamn_materializer::{
    DecideError, MAX_CAUSATION_DEPTH, RefuseReason, SkipReason, Verdict, VerifiedSourceEventId,
    decide, decide_derived, serviceable, sql::select_known_package_ids_sql,
    verified_derived_source_event_id, verified_source_event_id,
};

#[cfg(not(test))]
use bindings::events::{self, MessageHandle as Message};
#[cfg(test)]
use bindings::wamn::jetstream::registration::ConsumerConfig;
#[cfg(not(test))]
use bindings::wamn::jetstream::registration::{self, ConsumerConfig};
use bindings::wamn::postgres::client;
use bindings::wamn::postgres::types::{PgError, SqlValue};
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
    env_key: String,
    batch: u32,
    fetch_ms: u32,
    sweep_ms: u64,
    max_sweeps: u64,
    max_depth: u32,
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
            env_key: required("WAMN_MAT_ENV_KEY")?,
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
            max_depth: env_or("WAMN_MAT_MAX_DEPTH", &MAX_CAUSATION_DEPTH.to_string())
                .parse()
                .map_err(|error| format!("WAMN_MAT_MAX_DEPTH: {error}"))?,
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
    skip_entity: u64,
    skip_package_by_registration: BTreeMap<String, u64>,
    skip_op: u64,
    skip_foreign_tenant: u64,
    skip_condition_false: u64,
    refuse_depth: u64,
    refuse_tenant_unscopable: u64,
    refuse_old_image_absent: u64,
    refuse_condition_error: u64,
    refuse_source_package_identity_unknown: u64,
    held_registrations: u64,
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
            "skip-entity": self.skip_entity,
            "skip-package-mismatch-by-registration": &self.skip_package_by_registration,
            "skip-op": self.skip_op,
            "skip-foreign-tenant": self.skip_foreign_tenant,
            "skip-condition-false": self.skip_condition_false,
            "refuse-depth": self.refuse_depth,
            "refuse-tenant-unscopable": self.refuse_tenant_unscopable,
            "refuse-old-image-absent": self.refuse_old_image_absent,
            "refuse-condition-error": self.refuse_condition_error,
            "refuse-source-package-identity-unknown": self.refuse_source_package_identity_unknown,
            "held-registrations": self.held_registrations,
            "poison": self.poison,
            "retry": self.retry,
            "terminated": self.terminated,
            "termination-retry": self.termination_retry,
        })
        .to_string()
    }
}

fn pg_name(error: &PgError) -> String {
    match error {
        PgError::SerializationFailure => "serialization-failure".into(),
        PgError::ConnectionUnavailable => "connection-unavailable".into(),
        PgError::StatementTimeout => "statement-timeout".into(),
        PgError::RowLimitExceeded(limit) => format!("row-limit-exceeded({limit})"),
        PgError::UniqueViolation(constraint) => format!("unique-violation({constraint})"),
        PgError::ForeignKeyViolation(constraint) => {
            format!("foreign-key-violation({constraint})")
        }
        PgError::CheckViolation(constraint) => format!("check-violation({constraint})"),
        PgError::ExclusionViolation(constraint) => format!("exclusion-violation({constraint})"),
        PgError::PermissionDenied => "permission-denied".into(),
        PgError::QueryError((state, message)) => format!("query-error({state}: {message})"),
    }
}

struct Serving {
    registration: EventRegistration,
    enable_sequence: u64,
    condition: Option<wamn_materializer::CompiledCondition>,
    resident_env_key: String,
}

struct LoadedServings {
    servings: Vec<Serving>,
    known_packages: BTreeSet<String>,
}

fn nats_message_ids(headers: &[Header]) -> Vec<&str> {
    headers
        .iter()
        .filter(|header| header.name.eq_ignore_ascii_case("Nats-Msg-Id"))
        .map(|header| header.value.as_str())
        .collect()
}

async fn load_servings(counters: &mut Counters) -> Result<LoadedServings, String> {
    let package_rows = client::query(select_known_package_ids_sql(), Vec::new())
        .await
        .map_err(|error| pg_name(&error))?
        .rows;
    let mut known_packages = BTreeSet::new();
    for row in package_rows {
        let Some(SqlValue::Text(package_id)) = row.first() else {
            return Err("package identity row shape".into());
        };
        known_packages.insert(package_id.clone());
    }
    let document = registration::triggers()
        .await
        .map_err(|error| format!("{error:?}"))?;
    let rows: Vec<wamn_materializer::workflow::EventTrigger> =
        serde_json::from_str(&document).map_err(|error| error.to_string())?;
    let mut servings = Vec::with_capacity(rows.len());
    for row in rows {
        let registration = EventRegistration {
            schema_version: wamn_event_reg::SCHEMA_VERSION.to_owned(),
            registration_id: row.workflow_id,
            package_id: row.application,
            source_package_id: row.source_package,
            entity: row.entity,
            ops: row.ops,
            condition: row.condition,
            input: RegistrationInput::Event,
        };
        let registration_id = &registration.registration_id;
        let condition = match serviceable(&registration) {
            Ok(condition) => condition,
            Err(DecideError::UnserviceableCondition(reason)) => {
                counters.held_registrations += 1;
                eprintln!(
                    "wamn::materializer HELD registration {registration_id}: condition is not serviceable ({reason:?})"
                );
                continue;
            }
        };
        servings.push(Serving {
            registration,
            condition,
            resident_env_key: required("WAMN_MAT_ENV_KEY")?,
            enable_sequence: row.enable_sequence,
        });
    }
    Ok(LoadedServings {
        servings,
        known_packages,
    })
}

enum Preparation {
    Deliver { source_event_id: String },
    Ack,
    Nack,
    Terminate(&'static str),
}

#[derive(Debug, PartialEq)]
enum PreparationGate<'a> {
    Ready(&'a VerifiedSourceEventId),
    MissingSourceId,
    SourcePackageIdentityRefusal(&'a RefuseReason),
}

fn preparation_gate<'a>(
    source_event_id: Option<&'a VerifiedSourceEventId>,
    verdict: &'a Verdict,
) -> PreparationGate<'a> {
    match verdict {
        Verdict::Refuse(reason @ RefuseReason::SourcePackageIdentityUnknown { .. }) => {
            PreparationGate::SourcePackageIdentityRefusal(reason)
        }
        _ => source_event_id.map_or(PreparationGate::MissingSourceId, PreparationGate::Ready),
    }
}

fn record_skip(counters: &mut Counters, serving: &Serving, reason: SkipReason) {
    match reason {
        SkipReason::SourcePackageMismatch => {
            let identity = serving.registration.qualified_id();
            *counters
                .skip_package_by_registration
                .entry(identity)
                .or_default() += 1;
        }
        SkipReason::EntityMismatch => counters.skip_entity += 1,
        SkipReason::OpMismatch => counters.skip_op += 1,
        SkipReason::ForeignTenant => counters.skip_foreign_tenant += 1,
        SkipReason::ConditionFalse => counters.skip_condition_false += 1,
    }
}

fn record_refusal(counters: &mut Counters, reason: &RefuseReason) -> &'static str {
    match reason {
        RefuseReason::SourcePackageIdentityUnknown { .. } => {
            counters.refuse_source_package_identity_unknown += 1;
            "source-package-identity-unknown"
        }
        RefuseReason::DepthExceeded { .. } => {
            counters.refuse_depth += 1;
            "registration-depth-exceeded"
        }
        RefuseReason::TenantUnscopable => {
            counters.refuse_tenant_unscopable += 1;
            "registration-tenant-unscopable"
        }
        RefuseReason::OldImageAbsent => {
            counters.refuse_old_image_absent += 1;
            "registration-old-image-absent"
        }
        RefuseReason::ConditionError(_) => {
            counters.refuse_condition_error += 1;
            "registration-condition-error"
        }
    }
}

fn refuse_message(
    serving: &Serving,
    message: &Message,
    stream_seq: u64,
    counters: &mut Counters,
    reason: &RefuseReason,
) -> Preparation {
    let literal = record_refusal(counters, reason);
    eprintln!(
        "wamn::materializer REFUSED registration={} subject={} stream_seq={} reason={reason:?}",
        serving.registration.qualified_id(),
        message.message().subject,
        stream_seq
    );
    Preparation::Terminate(literal)
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

fn prepare_message(
    config: &Config,
    serving: &Serving,
    known_packages: &BTreeSet<String>,
    message: &Message,
    counters: &mut Counters,
) -> Preparation {
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
    let (source_event_id, verdict) = match &source {
        SourceEvent::Cdc(envelope) => {
            let source_event_id =
                verified_source_event_id(&config.project, &config.env, envelope, &message_ids);
            let verdict = decide(
                &serving.registration,
                serving.condition.as_ref(),
                envelope,
                known_packages,
                &config.env_key,
                source_event_id
                    .as_ref()
                    .map(|_| serving.resident_env_key.as_str()),
                config.max_depth,
            );
            (source_event_id, verdict)
        }
        SourceEvent::Derived(event) => (
            verified_derived_source_event_id(
                &config.env_key,
                &config.project,
                &config.env,
                event,
                &message_ids,
            ),
            decide_derived(
                &serving.registration,
                serving.condition.as_ref(),
                event,
                known_packages,
                &config.env_key,
                config.max_depth,
            ),
        ),
    };
    let source_event_id = match preparation_gate(source_event_id.as_ref(), &verdict) {
        PreparationGate::Ready(source_event_id) => source_event_id.as_str().to_string(),
        PreparationGate::SourcePackageIdentityRefusal(reason) => {
            return refuse_message(serving, message, stream_seq, counters, reason);
        }
        PreparationGate::MissingSourceId => {
            counters.poison += 1;
            eprintln!(
                "wamn::materializer REFUSED poison stream_seq={stream_seq}: Nats-Msg-Id is missing, duplicated, or inconsistent"
            );
            return Preparation::Terminate("poison-source-id");
        }
    };
    if stream_seq <= serving.enable_sequence {
        return Preparation::Ack;
    }
    match verdict {
        Verdict::Deliver(_) => Preparation::Deliver { source_event_id },
        Verdict::Skip(reason) => {
            record_skip(counters, serving, reason);
            Preparation::Ack
        }
        Verdict::Refuse(reason) => refuse_message(serving, message, stream_seq, counters, &reason),
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

async fn serve(
    config: &Config,
    servings: &[&Serving],
    known_packages: &BTreeSet<String>,
    counters: &mut Counters,
) {
    let registration = &servings[0].registration;
    let durable = wamn_materializer::workflow::entity_durable(
        &registration.source_package_id,
        &registration.entity,
    );
    let filter = format!(
        "evt.{}.{}.{}.{}.>",
        config.org,
        config.project,
        config.env,
        wamn_event_wire::subject_token(registration.entity.as_str())
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
        let mut source_event_id = None;
        let mut refused = false;
        for serving in servings {
            match prepare_message(config, serving, known_packages, &message, counters) {
                Preparation::Deliver {
                    source_event_id: id,
                } => source_event_id = Some(id),
                Preparation::Ack => {}
                Preparation::Nack => {
                    nack(&message, config, counters).await;
                    refused = true;
                    break;
                }
                Preparation::Terminate(reason) => {
                    terminate(&message, reason, counters).await;
                    refused = true;
                    break;
                }
            }
        }
        if refused {
            continue;
        }
        let contents = message.message();
        // Even an empty guest match set reaches the transaction: enabled rows
        // can change between this sweep and admission.
        let source_event_id = source_event_id.or_else(|| {
            let source = wamn_materializer::workflow::SourceEvent::decode(&contents.body).ok()?;
            let headers = contents.headers.clone().unwrap_or_default();
            source
                .identity(
                    &config.env_key,
                    &config.project,
                    &config.env,
                    &nats_message_ids(&headers),
                )
                .map(|id| id.as_str().to_owned())
        });
        let Some(source_event_id) = source_event_id else {
            terminate(&message, "poison-source-id", counters).await;
            continue;
        };
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
                eprintln!("wamn::materializer batch admission failed: {error:?}");
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
        "wamn::materializer up: stream={} filter=evt.{}.{}.{}.*.* env_key={} batch={} fetch_ms={} max_deliver={} max_sweeps={}",
        config.stream,
        config.org,
        config.project,
        config.env,
        config.env_key,
        config.batch,
        config.fetch_ms,
        config.max_deliver,
        config.max_sweeps
    );
    let mut counters = Counters::default();
    loop {
        counters.sweeps += 1;
        match load_servings(&mut counters).await {
            Ok(loaded) if loaded.servings.is_empty() => {
                bindings::wasi::clocks::monotonic_clock::wait_for(
                    config.sweep_ms.saturating_mul(1_000_000),
                )
                .await;
            }
            Ok(loaded) => {
                let mut entities = BTreeMap::<_, Vec<_>>::new();
                for serving in &loaded.servings {
                    entities
                        .entry((
                            &serving.registration.source_package_id,
                            &serving.registration.entity,
                        ))
                        .or_default()
                        .push(serving);
                }
                for servings in entities.values() {
                    serve(&config, servings, &loaded.known_packages, &mut counters).await;
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
            env_key: "tenant".into(),
            batch: 1,
            fetch_ms: 1,
            sweep_ms: 1,
            max_sweeps: 1,
            max_depth: MAX_CAUSATION_DEPTH,
            ack_wait_ms: 30000,
            nack_delay_ms: 1234,
            max_deliver: 5,
            report_path: None,
        }
    }

    fn message() -> Message {
        Message {
            contents: bindings::wasmcloud::nats::types::NatsMessage {
                subject: "evt.org.app.dev.receipts.insert".into(),
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

    fn serve_message(serving: &Serving, known: &BTreeSet<String>, message: Message) -> Counters {
        events::MESSAGES.set(vec![message]);
        registration::ADMITS.set(0);
        let mut counters = Counters::default();
        ready(serve(&config(), &[serving], known, &mut counters));
        counters
    }

    #[test]
    fn refused_message_terminates_without_admission() {
        let serving = serving();
        let message = message();
        let known = BTreeSet::new();
        assert!(matches!(
            prepare_message(
                &config(),
                &serving,
                &known,
                &message,
                &mut Counters::default()
            ),
            Preparation::Terminate("source-package-identity-unknown")
        ));
        let counters = serve_message(&serving, &known, message.clone());
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
        let counters = serve_message(
            &serving(),
            &BTreeSet::from(["receiving".into()]),
            message.clone(),
        );
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
    fn skipped_message_is_acked_after_host_admission() {
        let mut serving = serving();
        serving.registration.entity = "other".into();
        let message = message();
        let known = BTreeSet::from(["receiving".into()]);
        assert!(matches!(
            prepare_message(
                &config(),
                &serving,
                &known,
                &message,
                &mut Counters::default()
            ),
            Preparation::Ack
        ));
        let counters = serve_message(&serving, &known, message.clone());
        assert_eq!(*message.settlements.borrow(), vec![events::Settlement::Ack]);
        assert_eq!(registration::ADMITS.get(), 1);
        assert_eq!(counters.skip_entity, 1);
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
            condition: None,
            resident_env_key: "tenant".into(),
            enable_sequence: 0,
        }
    }

    fn envelope() -> Envelope {
        serde_json::from_value(serde_json::json!({
            "op": "insert",
            "new": {"tenant_id": "tenant"},
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
            "new": {"tenant_id": "tenant"},
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
            "tenant",
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

    #[test]
    fn unknown_source_package_refusal_has_a_typed_literal_and_counter() {
        let mut counters = Counters::default();
        assert_eq!(
            record_refusal(
                &mut counters,
                &RefuseReason::SourcePackageIdentityUnknown {
                    source_package_id: "uninstalled".into(),
                },
            ),
            "source-package-identity-unknown"
        );
        assert_eq!(counters.refuse_source_package_identity_unknown, 1);
    }

    #[test]
    fn known_package_mismatch_is_counted_per_registration() {
        let serving = serving();
        let known_packages = BTreeSet::from(["receiving".to_owned(), "overlay".to_owned()]);
        let mut event = envelope();
        event.package_id = "overlay".into();
        let Verdict::Skip(reason) = decide(
            &serving.registration,
            None,
            &event,
            &known_packages,
            "tenant",
            Some("tenant"),
            MAX_CAUSATION_DEPTH,
        ) else {
            panic!("a known foreign package must be normal filtration");
        };
        assert_eq!(reason, SkipReason::SourcePackageMismatch);
        let mut counters = Counters::default();
        record_skip(&mut counters, &serving, reason);
        assert_eq!(
            counters
                .skip_package_by_registration
                .get("client_acme_receiving::receive-receipt"),
            Some(&1)
        );
        assert_eq!(counters.skip_entity, 0);
    }
}
