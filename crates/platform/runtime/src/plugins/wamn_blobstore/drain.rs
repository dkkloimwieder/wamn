//! Draining a guest's `stream<u8>` into an [`Intake`], bounded and honest.
//!
//! Upstream has the same mechanism and draws the opposite conclusion. Its
//! `stream_collect` documents the hazard precisely — "a stream the guest
//! abandons part-way is indistinguishable from one it ended deliberately:
//! both yield whatever bytes arrived" — and then drains with [`usize::MAX`]
//! and writes whatever it got. Under §2c's caller-supplied deterministic key
//! and overwrite-safe put, that is how a partial body silently replaces a
//! complete one.
//!
//! # What is and is not distinguishable
//!
//! Being exact about this matters, because the guarantee is narrower than
//! "we detect truncation" and claiming the wider one would be false:
//!
//! * **Over the ceiling** — distinguishable. The excess is refused without
//!   buffering and reported as an error, not as a short body.
//! * **Collector abandoned** — distinguishable. The consumer went away without
//!   reporting; surfaced rather than read as an empty body.
//! * **Guest torn down mid-write** — distinguishable, structurally. The commit
//!   happens *after* this drain returns inside the host call, so a guest that
//!   traps or is cancelled never reaches it and nothing is written.
//! * **A guest that deliberately ends its stream early** — NOT distinguishable,
//!   and not truncation. The contract carries no length, so a guest writing
//!   fewer bytes than it intended is indistinguishable from one writing
//!   exactly what it meant to. No host-side wall can close that without a
//!   length the ratified contract does not have; it is the guest's own bug.
//!
//! The first three are the ones that would otherwise destroy a good object
//! under a deterministic key. Those are closed here.

use wash_runtime::wasmtime::component::{
    Accessor, HasData, Source, StreamConsumer, StreamReader, StreamResult,
};

use super::intake::{Intake, IntakeError};

/// Drain `stream` into an [`Intake`] under `limit`, returning the body only on
/// a clean end-of-stream.
///
/// # Errors
///
/// [`IntakeError::TooLarge`] when the body crosses the ceiling, naming the
/// limit and what was observed; [`IntakeError::Incomplete`] when the collector
/// was abandoned without reporting.
pub async fn drain_body<T, D>(
    accessor: &Accessor<T, D>,
    stream: StreamReader<u8>,
    limit: usize,
) -> wash_runtime::wasmtime::Result<Result<Vec<u8>, IntakeError>>
where
    T: 'static,
    D: HasData,
{
    let (done, finished) = tokio::sync::oneshot::channel::<Result<Vec<u8>, IntakeError>>();
    accessor.with(|mut access| {
        stream.pipe(
            &mut access,
            IntakeConsumer {
                intake: Intake::with_limit(limit),
                done: Some(done),
            },
        )
    })?;
    // A collector that vanished without sending is NOT an empty body. Reading
    // it as one is exactly how a zero-length object would overwrite a good one.
    //
    // The refusal says the observed count is UNKNOWN rather than reporting the
    // 0 an empty placeholder would carry: the bytes went with the collector,
    // and a confident "0 bytes" here would be a second, quieter lie about the
    // same event.
    Ok(finished.await.unwrap_or_else(|_| {
        Err(Intake::with_limit(limit).abort(
            "stream collector was dropped without reporting; bytes received is unknown",
        ))
    }))
}

/// Accumulates a body into an [`Intake`], releasing it only at end-of-stream.
struct IntakeConsumer {
    intake: Intake,
    done: Option<tokio::sync::oneshot::Sender<Result<Vec<u8>, IntakeError>>>,
}

impl Drop for IntakeConsumer {
    /// End-of-stream is observed here: the runtime drops the consumer when the
    /// stream ends. Reaching this with `done` still held means no error was
    /// reported along the way, so the body is complete as far as the transport
    /// can tell, and only then is it committed.
    fn drop(&mut self) {
        if let Some(done) = self.done.take() {
            let intake = std::mem::replace(&mut self.intake, Intake::with_limit(0));
            let _ = done.send(Ok(intake.commit()));
        }
    }
}

impl<D> StreamConsumer<D> for IntakeConsumer {
    type Item = u8;

    fn poll_consume(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
        store: wash_runtime::wasmtime::StoreContextMut<D>,
        source: Source<Self::Item>,
        finish: bool,
    ) -> std::task::Poll<wash_runtime::wasmtime::Result<StreamResult>> {
        let this = self.get_mut();
        let mut source = source.as_direct(store);
        let bytes = source.remaining();
        if bytes.is_empty() {
            // Nothing offered. An in-memory sink is always ready to accept;
            // the real end-of-stream arrives via `Drop`.
            return std::task::Poll::Ready(Ok(if finish {
                StreamResult::Cancelled
            } else {
                StreamResult::Completed
            }));
        }
        let count = bytes.len();
        if let Err(error) = this.intake.push(bytes) {
            // Report the ceiling HERE rather than through `Drop`, so the caller
            // sees the refusal instead of a body that merely looks short.
            // `Dropped` is wasmtime's disposition for a consumer that will
            // accept no more and reports its error by other means.
            if let Some(done) = this.done.take() {
                let _ = done.send(Err(error));
            }
            return std::task::Poll::Ready(Ok(StreamResult::Dropped));
        }
        source.mark_read(count);
        std::task::Poll::Ready(Ok(StreamResult::Completed))
    }
}
