//! Request deadline, Response handoff, and fenced durable settlement.

use std::pin::pin;

use futures::future::{Either, select};
use wit_bindgen::spawn_local;

use crate::bindings::exports::wamn::workflow::entry::{Request, Response};
use crate::bindings::wamn::workflow::run::{self, Completion, LeasedRun, Refusal};
use crate::bindings::wasi::clocks::monotonic_clock as clock;
use crate::graph::Graph;
use crate::walk::{Boundary, Walk, WalkError};

pub(crate) const NANOS_PER_SECOND: u64 = 1_000_000_000;

// Renew at one third of the host's 30-second lease TTL.
const RENEW_INTERVAL: u64 = 10 * NANOS_PER_SECOND;

pub(crate) async fn request(input: Request) -> Result<Response, Refusal> {
    let Request {
        handle,
        run_id,
        definition,
        input_json,
        remaining_ms,
    } = input;
    let deadline = clock::now().saturating_add(remaining_ms.saturating_mul(1_000_000));
    if remaining_ms == 0 {
        return Err(Refusal::DeadlineElapsed);
    }
    let durable = run_id.is_some();
    let leased_handle = durable.then_some(handle.as_str());
    let prefix = async {
        let prepared = Graph::parse(&definition).and_then(|graph| {
            if graph.request {
                let walk = Walk::start(&graph, input_json)?;
                Ok((graph, walk))
            } else {
                Err(WalkError::failed(
                    run::FailureType::InvalidInput,
                    "invalid-entry",
                    "Request call requires a Request graph",
                ))
            }
        });
        let (graph, mut walk) = match prepared {
            Ok(prepared) => prepared,
            Err(error) => return reject(leased_handle, &error).await,
        };
        if let Err(error) = walk.advance(&graph, &handle, Some(deadline)).await {
            return reject(leased_handle, &error).await;
        }
        if walk.pending() {
            let handle = leased_handle.ok_or(Refusal::Unavailable)?;
            checkpoint(handle, &walk).await?;
        } else if let Some(handle) = leased_handle {
            finish(handle, &mut walk).await?;
        }
        Ok((graph, walk))
    };

    let prefix = async {
        if durable {
            renew_while(&handle, prefix).await
        } else {
            prefix.await
        }
    };
    let (graph, mut walk) = match select(pin!(prefix), pin!(clock::wait_until(deadline))).await {
        Either::Left((result, _)) if clock::now() < deadline => result?,
        _ => return Err(Refusal::DeadlineElapsed),
    };
    if walk.pending() {
        let response = walk.response.as_ref().ok_or(Refusal::Unavailable)?;
        let reply = Response {
            status: response.status,
            body_json: response.body_json.as_ref().clone(),
        };
        spawn_local(async move {
            if let Err(error) = renew_while(&handle, drive(&graph, &handle, &mut walk)).await {
                eprintln!("workflow Response continuation refused: {error:?}");
            }
        });
        Ok(reply)
    } else {
        let response = walk.response.take().ok_or(Refusal::Unavailable)?;
        Ok(Response {
            status: response.status,
            body_json: std::rc::Rc::unwrap_or_clone(response.body_json),
        })
    }
}

pub(crate) async fn run(input: LeasedRun) -> Result<(), Refusal> {
    let LeasedRun {
        authority,
        definition,
        input_json,
        checkpoint_json,
        ..
    } = input;
    let work = async {
        let prepared = Graph::parse(&definition).and_then(|graph| {
            let walk = match checkpoint_json {
                Some(checkpoint) => Walk::resume(&checkpoint),
                None => Walk::start(&graph, input_json),
            }?;
            Ok((graph, walk))
        });
        match prepared {
            Ok((graph, mut walk)) => drive(&graph, &authority, &mut walk).await,
            Err(error) => finish_failure(&authority, &error, None).await,
        }
    };
    renew_while(&authority, work).await
}

async fn drive(graph: &Graph, handle: &str, walk: &mut Walk) -> Result<(), Refusal> {
    loop {
        match walk.advance(graph, handle, None).await {
            Ok(Boundary::Response) if walk.pending() => checkpoint(handle, walk).await?,
            Ok(Boundary::Response | Boundary::Complete) => return finish(handle, walk).await,
            Err(error) => return finish_failure(handle, &error, walk.response.as_ref()).await,
        }
    }
}

async fn renew_while<T>(
    handle: &str,
    work: impl Future<Output = Result<T, Refusal>>,
) -> Result<T, Refusal> {
    let heartbeat = async {
        loop {
            clock::wait_for(RENEW_INTERVAL).await;
            run::renew(handle.to_owned()).await?;
        }
    };
    match select(pin!(work), pin!(heartbeat)).await {
        Either::Left((result, _)) | Either::Right((result, _)) => result,
    }
}

async fn checkpoint(handle: &str, walk: &Walk) -> Result<(), Refusal> {
    let state =
        serde_json::to_string(walk).map_err(|source| serialization("checkpoint", &source))?;
    run::checkpoint(handle.to_owned(), state).await
}

async fn finish(handle: &str, walk: &mut Walk) -> Result<(), Refusal> {
    let result = walk
        .result_json()
        .map_err(|source| serialization("result", &source))?;
    run::finish(handle.to_owned(), Completion::Completed(result)).await
}

async fn finish_failure(
    handle: &str,
    error: &WalkError,
    response: Option<&crate::walk::Response>,
) -> Result<(), Refusal> {
    let completion = error
        .completion(response)
        .map_err(|source| serialization("failure", &source))?
        .ok_or_else(|| error.refusal())?;
    run::finish(handle.to_owned(), completion).await
}

async fn reject<T>(handle: Option<&str>, error: &WalkError) -> Result<T, Refusal> {
    if let Some(handle) = handle {
        finish_failure(handle, error, None).await?;
    }
    Err(error.refusal())
}

fn serialization(context: &str, source: &serde_json::Error) -> Refusal {
    eprintln!("encode workflow {context}: {source}");
    Refusal::Unavailable
}
