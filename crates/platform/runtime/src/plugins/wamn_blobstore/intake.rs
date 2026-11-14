//! Body intake for `write-data`: a size ceiling, and completion honesty.
//!
//! # Why this exists
//!
//! Upstream drains a guest's `stream<u8>` with `usize::MAX` and commits what
//! it got. Two things go wrong under WAMN's rules.
//!
//! **Unbounded buffering.** A project-facing effect that will hold an entire
//! object in host memory needs a stated ceiling, not the absence of one.
//!
//! **Silent truncation — the sharper half.** A stream that ends early is
//! byte-for-byte indistinguishable from one that completed. Combine that with
//! §2c's rule that the caller supplies a DETERMINISTIC key and `put` is an
//! overwrite, and a half-received body silently replaces a good object under
//! the same key, with no error anywhere. That is invisible loss at storage
//! grain: nothing fails, and the data is gone.
//!
//! # The seam
//!
//! `wasmcloud:blobstore@0.1.0` gives `write-data` no length parameter, so a
//! length-prefix is not available without deviating from a contract the owner
//! ratified as exact and single. The remaining honest option is EXPLICIT
//! COMMIT: buffer the body, and issue the store write only once the stream has
//! signalled clean end-of-stream. A stream that errors, or is dropped, or
//! exceeds the ceiling, never reaches the store at all — so a truncated body
//! cannot overwrite a complete one. The put operation commits only after
//! it receives the whole body.

/// Ceiling on a single object body, in bytes.
///
/// 64 MiB: comfortably above a rendered label or document, far below anything
/// that would threaten a host holding several concurrently. Raising it is a
/// deliberate change, which is why it is a named constant and not a literal.
pub const MAX_OBJECT_BYTES: usize = 64 * 1024 * 1024;

/// Why a body was refused before it reached the store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IntakeError {
    /// The body exceeded [`MAX_OBJECT_BYTES`]. Carries both the limit and what
    /// was observed, because "too big" without a number is unactionable.
    TooLarge {
        /// The ceiling in force.
        limit: usize,
        /// Bytes observed before intake stopped. At least `limit + 1`.
        observed: usize,
    },
    /// The stream ended without signalling clean completion. The body is
    /// possibly partial, so nothing is written.
    Incomplete {
        /// Bytes buffered before the stream failed.
        observed: usize,
        /// Why the stream ended, as reported by the transport.
        detail: String,
    },
}

impl core::fmt::Display for IntakeError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::TooLarge { limit, observed } => write!(
                formatter,
                "object body exceeds the {limit}-byte limit; observed at least {observed} bytes"
            ),
            Self::Incomplete { observed, detail } => write!(
                formatter,
                "object body ended without clean completion after {observed} bytes ({detail}); \
                 nothing was written, because a partial body would overwrite a complete one \
                 under the same deterministic key"
            ),
        }
    }
}

impl std::error::Error for IntakeError {}

/// Accumulates a body under the ceiling, committing only on clean completion.
///
/// Construct, [`push`](Intake::push) each chunk, then call exactly one of
/// [`commit`](Intake::commit) or [`abort`](Intake::abort). There is
/// deliberately no way to obtain the buffered bytes without `commit`.
#[derive(Debug)]
pub struct Intake {
    buffer: Vec<u8>,
    limit: usize,
}

impl Intake {
    /// A new intake at an explicit ceiling. Tests use this; production uses
    /// [`Intake::new`].
    #[must_use]
    pub fn with_limit(limit: usize) -> Self {
        Self {
            buffer: Vec::new(),
            limit,
        }
    }

    /// Accept one chunk.
    ///
    /// # Errors
    ///
    /// [`IntakeError::TooLarge`] once the body would exceed the ceiling. The
    /// intake stops accumulating at that point; the caller must not commit.
    pub fn push(&mut self, chunk: &[u8]) -> Result<(), IntakeError> {
        if self.buffer.len().saturating_add(chunk.len()) > self.limit {
            return Err(IntakeError::TooLarge {
                limit: self.limit,
                observed: self.buffer.len().saturating_add(chunk.len()),
            });
        }
        self.buffer.extend_from_slice(chunk);
        Ok(())
    }

    /// The stream signalled clean end-of-stream: release the body to be
    /// written. This is the ONLY way to obtain the bytes.
    #[must_use]
    pub fn commit(self) -> Vec<u8> {
        self.buffer
    }

    /// The stream ended without clean completion. Consumes the intake and
    /// yields the refusal; the buffered bytes are dropped unwritten.
    #[must_use]
    pub fn abort(self, detail: impl Into<String>) -> IntakeError {
        IntakeError::Incomplete {
            observed: self.buffer.len(),
            detail: detail.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_complete_body_commits_verbatim() {
        let mut intake = Intake::with_limit(MAX_OBJECT_BYTES);
        intake.push(b"^XA").expect("chunk fits");
        intake.push(b"^XZ").expect("chunk fits");
        assert_eq!(intake.commit(), b"^XA^XZ");
    }

    #[test]
    fn the_ceiling_refuses_with_the_limit_and_observed_size() {
        let mut intake = Intake::with_limit(4);
        intake.push(b"abcd").expect("exactly the limit fits");
        let error = intake.push(b"e").expect_err("one byte over must refuse");
        assert_eq!(
            error,
            IntakeError::TooLarge {
                limit: 4,
                observed: 5
            }
        );
    }

    #[test]
    fn a_chunk_that_would_cross_the_ceiling_is_refused_whole() {
        let mut intake = Intake::with_limit(4);
        assert!(intake.push(b"abcdef").is_err());
        assert!(intake.commit().is_empty());
    }

    /// The invisible-loss guard. A truncated stream yields a refusal that
    /// names the partial size — and, crucially, no bytes, so there is nothing
    /// a caller could mistakenly write over a good object.
    #[test]
    fn a_truncated_stream_yields_a_refusal_and_no_bytes() {
        let mut intake = Intake::with_limit(MAX_OBJECT_BYTES);
        intake.push(b"^XA half a label").expect("chunk fits");
        let error = intake.abort("peer reset");

        match &error {
            IntakeError::Incomplete { observed, .. } => {
                assert_eq!(*observed, 16);
            }
            other @ IntakeError::TooLarge { .. } => panic!("expected Incomplete, got {other:?}"),
        }
    }

    /// The type makes the rule structural rather than advisory: bytes are
    /// reachable ONLY through `commit`, and `abort` consumes the intake, so a
    /// truncated body cannot be written by forgetting a test.
    #[test]
    fn the_only_route_to_the_bytes_is_commit() {
        let mut intake = Intake::with_limit(MAX_OBJECT_BYTES);
        intake.push(b"partial").expect("chunk fits");
        // `abort` consumes `intake`; nothing hands back the buffer.
        let error = intake.abort("stream dropped");
        assert!(matches!(error, IntakeError::Incomplete { .. }));
    }
}
