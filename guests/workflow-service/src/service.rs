//! Drive UTC schedules, expired leases, and the application's pull consumers.

use std::cell::Cell;
use std::pin::pin;
use std::rc::Rc;

use futures::future::{Either, join3, select};
use wit_bindgen::spawn_local;

use crate::bindings::wamn::workflow::run::{self, DataChange, DataChangeConsumer, LeasedRun};
use crate::bindings::wasi::clocks::{monotonic_clock as clock, system_clock};
use crate::bindings::wasmcloud::nats::jetstream::{self, MessageHandle, NatsError};
use crate::delivery::{self, NANOS_PER_SECOND};

const UTC_MINUTE: i64 = 60;
const MINUTE_NS: u64 = 60 * NANOS_PER_SECOND;
const RECOVERY_INTERVAL: u64 = 30 * NANOS_PER_SECOND;
const CONSUMER_REFRESH: u64 = NANOS_PER_SECOND;
const FETCH_TIMEOUT_MS: u32 = 1_000;
// The host's pull message handle retains its progress acker after settlement.
const PROGRESS_INTERVAL: u64 = 10 * NANOS_PER_SECOND;

pub(crate) async fn serve() {
    join3(schedules(), recover(), consumers()).await;
}

async fn schedules() {
    let mut previous = None;
    loop {
        let now = system_clock::now();
        let elapsed = now.seconds.rem_euclid(UTC_MINUTE);
        let minute = now.seconds.saturating_sub(elapsed);
        let elapsed = elapsed.unsigned_abs() * NANOS_PER_SECOND + u64::from(now.nanoseconds);
        let next = clock::now().saturating_add(MINUTE_NS - elapsed);
        if previous.is_none_or(|previous| minute > previous) {
            previous = Some(minute);
            match run::admit_schedules(minute).await {
                Ok(runs) => {
                    for run in runs {
                        spawn_local(run_leased(run));
                    }
                }
                Err(error) => eprintln!("admit workflow schedules: {error:?}"),
            }
        }
        clock::wait_until(next).await;
    }
}

async fn recover() {
    let mut next = clock::now().saturating_add(RECOVERY_INTERVAL);
    loop {
        clock::wait_until(next).await;
        next = clock::now().saturating_add(RECOVERY_INTERVAL);
        loop {
            match run::take_expired().await {
                Ok(Some(run)) => spawn_local(run_leased(run)),
                Ok(None) => break,
                Err(error) => {
                    eprintln!("take expired workflow lease: {error:?}");
                    break;
                }
            }
        }
    }
}

async fn run_leased(run: LeasedRun) {
    if let Err(error) = delivery::run(run).await {
        eprintln!("execute leased workflow: {error:?}");
    }
}

struct Worker {
    spec: DataChangeConsumer,
    enabled: Cell<bool>,
}

fn same_consumer(a: &DataChangeConsumer, b: &DataChangeConsumer) -> bool {
    a.workflow_id == b.workflow_id && a.stream == b.stream && a.consumer == b.consumer
}

async fn consumers() {
    let mut workers = Vec::<Rc<Worker>>::new();
    loop {
        match run::data_change_consumers().await {
            Ok(enabled) => {
                workers.retain(|worker| {
                    let active = enabled.iter().any(|spec| same_consumer(spec, &worker.spec));
                    worker.enabled.set(active);
                    active
                });
                for spec in enabled {
                    if workers
                        .iter()
                        .any(|worker| same_consumer(&spec, &worker.spec))
                    {
                        continue;
                    }
                    let worker = Rc::new(Worker {
                        spec,
                        enabled: Cell::new(true),
                    });
                    workers.push(Rc::clone(&worker));
                    spawn_local(pull(worker));
                }
            }
            Err(error) => eprintln!("list data-change workflow consumers: {error:?}"),
        }
        clock::wait_for(CONSUMER_REFRESH).await;
    }
}

async fn pull(worker: Rc<Worker>) {
    while worker.enabled.get() {
        match jetstream::open_pull_consumer(
            worker.spec.stream.clone(),
            worker.spec.consumer.clone(),
        )
        .await
        {
            Ok(consumer) => {
                while worker.enabled.get() {
                    match consumer.fetch(1, FETCH_TIMEOUT_MS).await {
                        Ok(batch) => {
                            for message in batch.messages {
                                process_message(&worker.spec.workflow_id, &message).await;
                            }
                        }
                        Err(NatsError::NoMessages) => {}
                        Err(error) => {
                            eprintln!("fetch data-change workflow event: {error:?}");
                            break;
                        }
                    }
                }
            }
            Err(error) => eprintln!("open data-change workflow consumer: {error:?}"),
        }
        clock::wait_for(CONSUMER_REFRESH).await;
    }
}

async fn process_message(workflow_id: &str, handle: &MessageHandle) {
    let work = async {
        let message = handle.message();
        let message_ids = message
            .headers
            .into_iter()
            .flatten()
            .filter_map(|header| {
                header
                    .name
                    .eq_ignore_ascii_case("Nats-Msg-Id")
                    .then_some(header.value)
            })
            .collect();
        let admission = run::admit_data_change(DataChange {
            workflow_id: workflow_id.into(),
            subject: message.subject,
            body: message.body,
            message_ids,
            stream_sequence: handle.sequence(),
        })
        .await;
        let leased = match admission {
            Ok(leased) => leased,
            Err(error) => {
                eprintln!("admit data-change workflow event: {error:?}");
                return;
            }
        };
        if let Err(error) = handle.ack().await {
            eprintln!("acknowledge admitted workflow event: {error:?}");
            return;
        }
        if let Some(run) = leased {
            run_leased(run).await;
        }
    };
    let progress = async {
        loop {
            clock::wait_for(PROGRESS_INTERVAL).await;
            if let Err(error) = handle.in_progress().await {
                eprintln!("extend data-change acknowledgement wait: {error:?}");
            }
        }
    };
    match select(pin!(work), pin!(progress)).await {
        Either::Left(((), _)) => {}
        Either::Right(_) => unreachable!("the progress loop does not return"),
    }
}
