//! One application and one device loop on a small box (docs/plan/edge.md).
//!
//! This crate exports:
//!
//! - [`release`]: [`EdgeRelease`](release::EdgeRelease), a platform-published
//!   release loaded from a local directory and pinned by one bundle digest.
//! - [`grants`]: [`Grants`](grants::Grants), the permissions of each role on
//!   the box.
//! - [`serve`]: [`serve`](serve::serve), which loads the release and serves
//!   its routes behind a local ingress.
//! - [`authenticator`], [`delivery`], [`policy`] and [`application`]: the
//!   edge side of the engine's route and operation traits.
//! - [`intents`]: the `wamn-edge intents` commands, which list and resolve
//!   uncertain intents while the edge is stopped.
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

pub mod application;
pub mod authenticator;
pub mod delivery;
pub mod grants;
pub mod intents;
pub mod policy;
pub mod release;
pub mod serve;
