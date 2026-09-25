//! One application and one device loop on a small box (docs/plan/edge.md).
//!
//! This crate exports nothing yet.
//!
//! This crate refuses to depend on:
//!
//! - Upper layers: `wamn-runtime`, `wamn-workflow`, `wamn-router`,
//!   `wamn-execution-host`.
//! - Postgres: `tokio-postgres`, `deadpool-postgres`, `postgres-types`, and
//!   `wamn-platform-identity`, which links `tokio-postgres`.
//! - Network services: `async-nats`, `object_store`, `redis`.
//! - OCI: `oci-client`, `oci-wasm`.
//! - OTLP gRPC: `opentelemetry-otlp`, `tonic`.
//!
//! wash-runtime links `async-nats`, `redis`, `tonic`, `opentelemetry-otlp`,
//! `hyper-util`, and `hyper-rustls` with every feature off. The owner accepted
//! that list for the edge as for the engine (finding `wamn-qt1t`, spec section
//! 4.1), so the dependency test ignores what is reached only through
//! wash-runtime and pins that list. If its size costs the Pi too much, the
//! answer is a wash-runtime fork with those features off.

use wamn_engine as _;
use wamn_run_state as _;
