//! Concrete bounded adapters over the frozen `wamn:node/payloads@0.1.0` import.

use core::fmt;

use wamn_node_sdk as sdk;

mod bindings {
    wit_bindgen::generate!({
        world: "payload-api",
        path: "wit-payload",
        generate_all,
    });
}

use bindings::wamn::node::{payloads, types};
use bindings::wasi::io::streams::{InputStream, OutputStream, StreamError};

/// A reader tied to one exact run-scoped payload reference.
pub struct Reader {
    reference: sdk::PayloadRef,
    stream: InputStream,
}

impl fmt::Debug for Reader {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Reader")
            .field("reference", &self.reference)
            .finish_non_exhaustive()
    }
}

impl sdk::PayloadReader for Reader {
    fn reference(&self) -> &sdk::PayloadRef {
        &self.reference
    }

    fn read_chunk(&mut self) -> Result<Option<sdk::PayloadChunk>, sdk::PayloadStreamError> {
        match self
            .stream
            .blocking_read(sdk::MAX_PAYLOAD_CHUNK_BYTES as u64)
        {
            Ok(bytes) if bytes.is_empty() => Err(sdk::PayloadStreamError::EmptyChunk),
            Ok(bytes) => sdk::PayloadChunk::new(bytes)
                .map(Some)
                .map_err(|error| match error {
                    sdk::PayloadChunkError::Empty => sdk::PayloadStreamError::EmptyChunk,
                    sdk::PayloadChunkError::TooLarge { actual, maximum } => {
                        sdk::PayloadStreamError::OversizedChunk { actual, maximum }
                    }
                }),
            Err(StreamError::Closed) => Ok(None),
            Err(error) => Err(stream_error_to_sdk(error)),
        }
    }
}

/// A writer tied to one newly created run-scoped payload reference.
pub struct Writer {
    reference: sdk::PayloadRef,
    stream: OutputStream,
}

impl fmt::Debug for Writer {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Writer")
            .field("reference", &self.reference)
            .finish_non_exhaustive()
    }
}

impl sdk::PayloadWriter for Writer {
    fn reference(&self) -> &sdk::PayloadRef {
        &self.reference
    }

    fn write_chunk(&mut self, chunk: &sdk::PayloadChunk) -> Result<(), sdk::PayloadStreamError> {
        self.stream
            .blocking_write_and_flush(chunk.as_bytes())
            .map_err(stream_error_to_sdk)
    }

    fn flush(&mut self) -> Result<(), sdk::PayloadStreamError> {
        self.stream.blocking_flush().map_err(stream_error_to_sdk)
    }
}

/// Open one exact payload reference for bounded reads.
pub fn read(reference: &sdk::PayloadRef) -> Result<Reader, sdk::PayloadError> {
    let stream = payloads::read(&reference_to_wit(reference)).map_err(payload_error_to_sdk)?;
    Ok(Reader {
        reference: reference.clone(),
        stream,
    })
}

/// Create a payload with the selected framing for bounded writes.
pub fn create(framing: sdk::Framing) -> Result<Writer, sdk::PayloadError> {
    let (reference, stream) =
        payloads::create(framing_to_wit(framing)).map_err(payload_error_to_sdk)?;
    Ok(Writer {
        reference: reference_to_sdk(reference),
        stream,
    })
}

fn framing_to_wit(framing: sdk::Framing) -> types::Framing {
    match framing {
        sdk::Framing::Ndjson => types::Framing::Ndjson,
        sdk::Framing::Raw => types::Framing::Raw,
    }
}

fn framing_to_sdk(framing: types::Framing) -> sdk::Framing {
    match framing {
        types::Framing::Ndjson => sdk::Framing::Ndjson,
        types::Framing::Raw => sdk::Framing::Raw,
    }
}

fn reference_to_wit(reference: &sdk::PayloadRef) -> types::PayloadRef {
    types::PayloadRef {
        handle: reference.handle.clone(),
        framing: framing_to_wit(reference.framing),
        size_hint: reference.size_hint,
    }
}

fn reference_to_sdk(reference: types::PayloadRef) -> sdk::PayloadRef {
    sdk::PayloadRef {
        handle: reference.handle,
        framing: framing_to_sdk(reference.framing),
        size_hint: reference.size_hint,
    }
}

fn payload_error_to_sdk(error: payloads::PayloadError) -> sdk::PayloadError {
    match error {
        payloads::PayloadError::NotFound => sdk::PayloadError::NotFound,
        payloads::PayloadError::LimitExceeded(limit) => sdk::PayloadError::LimitExceeded(limit),
        payloads::PayloadError::Unavailable => sdk::PayloadError::Unavailable,
    }
}

fn stream_error_to_sdk(error: StreamError) -> sdk::PayloadStreamError {
    match error {
        StreamError::LastOperationFailed(error) => {
            sdk::PayloadStreamError::LastOperationFailed(error.to_debug_string())
        }
        StreamError::Closed => sdk::PayloadStreamError::Closed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn payload_reference_round_trips_with_framing_and_size_hint() {
        for framing in [sdk::Framing::Ndjson, sdk::Framing::Raw] {
            let reference = sdk::PayloadRef {
                handle: "run-scoped-handle".to_string(),
                framing,
                size_hint: Some(8192),
            };
            assert_eq!(reference_to_sdk(reference_to_wit(&reference)), reference);
        }
    }

    #[test]
    fn framing_maps_variant_for_variant() {
        assert!(matches!(
            framing_to_wit(sdk::Framing::Ndjson),
            types::Framing::Ndjson
        ));
        assert!(matches!(
            framing_to_wit(sdk::Framing::Raw),
            types::Framing::Raw
        ));
    }

    #[test]
    fn payload_open_and_create_errors_map_variant_for_variant() {
        assert_eq!(
            payload_error_to_sdk(payloads::PayloadError::NotFound),
            sdk::PayloadError::NotFound
        );
        assert_eq!(
            payload_error_to_sdk(payloads::PayloadError::LimitExceeded(17)),
            sdk::PayloadError::LimitExceeded(17)
        );
        assert_eq!(
            payload_error_to_sdk(payloads::PayloadError::Unavailable),
            sdk::PayloadError::Unavailable
        );
    }

    #[test]
    fn closed_write_remains_a_typed_stream_error() {
        assert_eq!(
            stream_error_to_sdk(StreamError::Closed),
            sdk::PayloadStreamError::Closed
        );
    }
}
