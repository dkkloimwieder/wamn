//! Bounded streamed-payload types shared by node implementations and guests.

use core::fmt;

/// The largest chunk a node payload API reads or writes in one operation.
///
/// This matches the maximum accepted by WASI P2
/// `output-stream.blocking-write-and-flush`.
pub const MAX_PAYLOAD_CHUNK_BYTES: usize = 4096;

/// How a streamed payload's bytes are framed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Framing {
    /// Newline-delimited JSON records in write order.
    Ndjson,
    /// Opaque bytes.
    Raw,
}

/// An opaque reference into the active run's payload store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PayloadRef {
    pub handle: String,
    pub framing: Framing,
    pub size_hint: Option<u64>,
}

/// Why opening or creating a payload failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PayloadError {
    NotFound,
    LimitExceeded(u64),
    Unavailable,
}

/// Why a P2 stream operation failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PayloadStreamError {
    /// The host reported an underlying operation failure.
    LastOperationFailed(String),
    /// The stream closed while an operation still required it to be open.
    Closed,
    /// A blocking read returned no bytes without reporting end-of-stream.
    EmptyChunk,
    /// The host returned more bytes than the requested bounded chunk size.
    OversizedChunk { actual: usize, maximum: usize },
}

/// Why bytes could not form one bounded payload chunk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PayloadChunkError {
    Empty,
    TooLarge { actual: usize, maximum: usize },
}

impl fmt::Display for PayloadChunkError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => formatter.write_str("a payload chunk must not be empty"),
            Self::TooLarge { actual, maximum } => {
                write!(
                    formatter,
                    "payload chunk has {actual} bytes; maximum is {maximum}"
                )
            }
        }
    }
}

/// One non-empty, bounded transfer unit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PayloadChunk(Box<[u8]>);

impl PayloadChunk {
    /// Validate bytes as one bounded transfer unit.
    pub fn new(bytes: impl Into<Box<[u8]>>) -> Result<Self, PayloadChunkError> {
        let bytes = bytes.into();
        match bytes.len() {
            0 => Err(PayloadChunkError::Empty),
            actual if actual > MAX_PAYLOAD_CHUNK_BYTES => Err(PayloadChunkError::TooLarge {
                actual,
                maximum: MAX_PAYLOAD_CHUNK_BYTES,
            }),
            _ => Ok(Self(bytes)),
        }
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    pub fn into_bytes(self) -> Box<[u8]> {
        self.0
    }
}

/// A bounded reader over one exact payload reference.
pub trait PayloadReader {
    fn reference(&self) -> &PayloadRef;

    /// Read at most [`MAX_PAYLOAD_CHUNK_BYTES`], or `None` at end-of-stream.
    fn read_chunk(&mut self) -> Result<Option<PayloadChunk>, PayloadStreamError>;
}

/// A bounded writer for one newly created payload reference.
pub trait PayloadWriter {
    fn reference(&self) -> &PayloadRef;

    /// Write exactly one already-bounded chunk.
    fn write_chunk(&mut self, chunk: &PayloadChunk) -> Result<(), PayloadStreamError>;

    /// Flush all bytes accepted so far.
    fn flush(&mut self) -> Result<(), PayloadStreamError>;
}

#[cfg(test)]
mod tests {
    use super::{MAX_PAYLOAD_CHUNK_BYTES, PayloadChunk, PayloadChunkError};

    #[test]
    fn payload_chunk_accepts_only_nonempty_bounded_transfers() {
        assert_eq!(
            PayloadChunk::new(Vec::<u8>::new()),
            Err(PayloadChunkError::Empty)
        );
        let maximum = vec![7; MAX_PAYLOAD_CHUNK_BYTES];
        assert_eq!(
            PayloadChunk::new(maximum.clone()).unwrap().as_bytes(),
            maximum
        );
        assert_eq!(
            PayloadChunk::new(vec![0; MAX_PAYLOAD_CHUNK_BYTES + 1]),
            Err(PayloadChunkError::TooLarge {
                actual: MAX_PAYLOAD_CHUNK_BYTES + 1,
                maximum: MAX_PAYLOAD_CHUNK_BYTES,
            })
        );
    }

    #[test]
    fn payload_api_exposes_no_whole_object_collector() {
        let source = include_str!("payload.rs");
        for forbidden in [
            concat!("read_", "to_end"),
            concat!("collect_", "payload"),
            concat!("into_payload_", "bytes"),
        ] {
            assert!(
                !source.contains(forbidden),
                "whole-object API leaked: {forbidden}"
            );
        }
    }
}
