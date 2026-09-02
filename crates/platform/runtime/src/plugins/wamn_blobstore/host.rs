//! The sixteen contract method bodies.
//!
//! Every one delegates to [`BoundContainer`], so the bucket and prefix walls
//! are applied on one path. Nothing here talks to an object store directly and
//! nothing here constructs a WIT error except through [`wit_error::to_wit`].
//!
//! # What a component can and cannot reach
//!
//! The binding fixes one container. A component therefore sees exactly one,
//! and the store-level verbs are shaped by that rather than by the contract's
//! generality:
//!
//! * `create-container` and `delete-container` are refused — the environment
//!   owns the container, and a guest that could create or destroy one would
//!   own its own authority.
//! * `get-container` and `container-exists` answer only for the bound name.
//!   A different name reports `no-such-container` / `false` rather than
//!   `access-denied`, because "that exists but is not yours" is itself a leak:
//!   it tells a guest what else lives in the store.
//! # Three verbs never succeed, in TWO categories
//!
//! The distinction is recorded because the remedies differ (owner ruling):
//!
//! * **Refused by policy** — `copy-object`, `move-object`. Their `object-id`
//!   arguments are bare strings carrying no backend or binding discriminator,
//!   so confinement cannot hold across them. A policy change brings them back,
//!   as binding-scoped variants, never these signatures.
//! * **Unsatisfiable by backend** — `info`. The object-store surface carries
//!   no container creation time, and `container-metadata` requires one.
//!   Fabricating a `0` would be a timestamp a guest could act on, which is the
//!   fabricated-evidence rule at data grain. This one returns if `object_store`
//!   grows the capability; no policy decision is involved.
//!
//! `create-container` and `delete-container` are neither: they are refused
//! because the environment owns the container, which is the ordinary operation
//! of the ownership split rather than a deviation from the contract.

use wash_runtime::engine::ctx::SharedCtx;
use wash_runtime::wasmtime::component::{Accessor, Resource};

use super::bindings::wasmcloud::blobstore::container::{Container, HostContainer, HostContainerWithStore};
use super::bindings::wasmcloud::blobstore::blobstore::HostWithStore;
use super::bindings::wasmcloud::blobstore::types::{
    ContainerMetadata, Error as WitError, ObjectId, ObjectMetadata,
};
use super::drain::drain_body;
use super::intake::MAX_OBJECT_BYTES;
use super::store::{BoundContainer, StoreError};
use super::wit_error::to_wit;

/// The refusal a store-owned verb returns.
fn refused(verb: &'static str, reason: &'static str) -> WitError {
    to_wit(&StoreError::Refused { verb, reason })
}

/// Read the bound container out of the resource table.
fn container_of<T>(
    accessor: &Accessor<T, SharedCtx>,
    handle: &Resource<Container>,
) -> wash_runtime::wasmtime::Result<BoundContainer>
where
    T: 'static,
{
    accessor.with(|mut access| {
        access
            .get()
            .table
            .get(handle)
            .cloned()
            .map_err(Into::into)
    })
}

impl HostContainer for SharedCtx {
    async fn drop(&mut self, handle: Resource<Container>) -> wash_runtime::wasmtime::Result<()> {
        self.table.delete(handle)?;
        Ok(())
    }
}

impl<T: 'static + Send> HostContainerWithStore<T> for SharedCtx {
    async fn name(
        accessor: &Accessor<T, Self>,
        handle: Resource<Container>,
    ) -> wash_runtime::wasmtime::Result<Result<String, WitError>> {
        Ok(Ok(container_of(accessor, &handle)?.container().to_owned()))
    }

    /// Container metadata is not available from an object store.
    ///
    /// `container-metadata` requires a `created-at`, and the S3 API surface
    /// `object_store` exposes carries no bucket creation time. Reporting a
    /// fabricated `0` would be a timestamp a guest could reasonably act on, so
    /// this reports unavailability instead.
    async fn info(
        _accessor: &Accessor<T, Self>,
        _handle: Resource<Container>,
    ) -> wash_runtime::wasmtime::Result<Result<ContainerMetadata, WitError>> {
        Ok(Err(refused(
            "info",
            "an object store exposes no container creation time, and a fabricated one would be \
             a timestamp a guest could act on",
        )))
    }

    async fn get_data(
        accessor: &Accessor<T, Self>,
        handle: Resource<Container>,
        name: String,
        start: u64,
        end: u64,
    ) -> wash_runtime::wasmtime::Result<Result<wash_runtime::wasmtime::component::StreamReader<u8>, WitError>> {
        let container = container_of(accessor, &handle)?;
        let body = match container.get(&name).await {
            Ok(body) => body,
            Err(error) => return Ok(Err(to_wit(&error))),
        };
        // Offsets are inclusive per the contract. A range outside the object is
        // an empty read rather than an error, matching a byte-range read.
        let from = usize::try_from(start).unwrap_or(usize::MAX).min(body.len());
        let to = usize::try_from(end)
            .unwrap_or(usize::MAX)
            .saturating_add(1)
            .min(body.len());
        let slice = body.get(from..to).unwrap_or_default().to_vec();
        let reader = accessor.with(|mut access| {
            wash_runtime::wasmtime::component::StreamReader::new(&mut access, slice)
        })?;
        Ok(Ok(reader))
    }

    async fn write_data(
        accessor: &Accessor<T, Self>,
        handle: Resource<Container>,
        name: String,
        data: wash_runtime::wasmtime::component::StreamReader<u8>,
    ) -> wash_runtime::wasmtime::Result<Result<(), WitError>> {
        let container = container_of(accessor, &handle)?;
        // The body is proven complete BEFORE the store is touched. A truncated
        // stream never reaches `put`, so it cannot overwrite a good object
        // under the caller's deterministic key.
        let body = match drain_body(accessor, data, MAX_OBJECT_BYTES).await? {
            Ok(body) => body,
            Err(error) => return Ok(Err(to_wit(&StoreError::Intake(error)))),
        };
        Ok(container.put(&name, body).await.map_err(|error| to_wit(&error)))
    }

    async fn list_objects(
        accessor: &Accessor<T, Self>,
        handle: Resource<Container>,
    ) -> wash_runtime::wasmtime::Result<Result<wash_runtime::wasmtime::component::StreamReader<String>, WitError>> {
        let container = container_of(accessor, &handle)?;
        let keys = match container.list().await {
            Ok(keys) => keys,
            Err(error) => return Ok(Err(to_wit(&error))),
        };
        let reader = accessor.with(|mut access| {
            wash_runtime::wasmtime::component::StreamReader::new(&mut access, keys)
        })?;
        Ok(Ok(reader))
    }

    async fn delete_object(
        accessor: &Accessor<T, Self>,
        handle: Resource<Container>,
        name: String,
    ) -> wash_runtime::wasmtime::Result<Result<(), WitError>> {
        let container = container_of(accessor, &handle)?;
        Ok(container.delete(&name).await.map_err(|error| to_wit(&error)))
    }

    async fn delete_objects(
        accessor: &Accessor<T, Self>,
        handle: Resource<Container>,
        names: Vec<String>,
    ) -> wash_runtime::wasmtime::Result<Result<(), WitError>> {
        let container = container_of(accessor, &handle)?;
        // Each key is resolved and refused on its own, so one containment
        // breach in a batch cannot ride in behind valid siblings.
        for name in &names {
            if let Err(error) = container.delete(name).await {
                return Ok(Err(to_wit(&error)));
            }
        }
        Ok(Ok(()))
    }

    async fn has_object(
        accessor: &Accessor<T, Self>,
        handle: Resource<Container>,
        name: String,
    ) -> wash_runtime::wasmtime::Result<Result<bool, WitError>> {
        let container = container_of(accessor, &handle)?;
        Ok(container.has(&name).await.map_err(|error| to_wit(&error)))
    }

    /// `created-at` reports the store's last-modified time.
    ///
    /// An object store keeps no separate creation time, and `put` is an
    /// overwrite, so for the version being described the two coincide. The
    /// approximation is named here rather than left for a reader to infer.
    ///
    /// **This holds only while `put` is the sole writer.** A multipart or
    /// append path would let an object be modified after creation, at which
    /// point last-modified stops being its creation time and this must change
    /// with it.
    async fn object_info(
        accessor: &Accessor<T, Self>,
        handle: Resource<Container>,
        name: String,
    ) -> wash_runtime::wasmtime::Result<Result<ObjectMetadata, WitError>> {
        let container = container_of(accessor, &handle)?;
        match container.head(&name).await {
            Ok(meta) => Ok(Ok(ObjectMetadata {
                name,
                container: container.container().to_owned(),
                created_at: meta.last_modified_unix_nanos,
                size: meta.size,
            })),
            Err(error) => Ok(Err(to_wit(&error))),
        }
    }

    /// Clearing removes every object under the BOUND PREFIX, never the
    /// container: a component that cannot name the container must not be able
    /// to empty it for everyone sharing it.
    async fn clear(
        accessor: &Accessor<T, Self>,
        handle: Resource<Container>,
    ) -> wash_runtime::wasmtime::Result<Result<(), WitError>> {
        let container = container_of(accessor, &handle)?;
        Ok(container.clear().await.map_err(|error| to_wit(&error)))
    }
}

impl<T: 'static + Send> HostWithStore<T> for SharedCtx {
    async fn create_container(
        _accessor: &Accessor<T, Self>,
        _name: String,
    ) -> wash_runtime::wasmtime::Result<Result<Resource<Container>, WitError>> {
        Ok(Err(refused(
            "create-container",
            "the environment owns the container; a guest that could create one would own its own \
             authority",
        )))
    }

    async fn get_container(
        accessor: &Accessor<T, Self>,
        name: String,
    ) -> wash_runtime::wasmtime::Result<Result<Resource<Container>, WitError>> {
        let Some(container) = bound_container(accessor, &name)? else {
            return Ok(Err(WitError::NoSuchContainer));
        };
        let handle = accessor.with(|mut access| access.get().table.push(container))?;
        Ok(Ok(handle))
    }

    async fn delete_container(
        _accessor: &Accessor<T, Self>,
        _name: String,
    ) -> wash_runtime::wasmtime::Result<Result<(), WitError>> {
        Ok(Err(refused(
            "delete-container",
            "the environment owns the container; a guest cannot destroy the store it was lent",
        )))
    }

    async fn container_exists(
        accessor: &Accessor<T, Self>,
        name: String,
    ) -> wash_runtime::wasmtime::Result<Result<bool, WitError>> {
        Ok(Ok(bound_container(accessor, &name)?.is_some()))
    }

    async fn copy_object(
        _accessor: &Accessor<T, Self>,
        _src: ObjectId,
        _dest: ObjectId,
    ) -> wash_runtime::wasmtime::Result<Result<(), WitError>> {
        Ok(Err(refused("copy-object", REFUSED_OBJECT_ID_REASON)))
    }

    async fn move_object(
        _accessor: &Accessor<T, Self>,
        _src: ObjectId,
        _dest: ObjectId,
    ) -> wash_runtime::wasmtime::Result<Result<(), WitError>> {
        Ok(Err(refused("move-object", REFUSED_OBJECT_ID_REASON)))
    }
}

/// Why the two object-id verbs are refused. One literal, so both refusals
/// carry the same reason and neither can drift.
pub const REFUSED_OBJECT_ID_REASON: &str =
    "object ids are bare strings carrying no backend or binding discriminator, so bucket and \
     prefix confinement cannot hold across them";

/// The bound container, if `name` is the one this component was lent.
fn bound_container<T>(
    _accessor: &Accessor<T, SharedCtx>,
    _name: &str,
) -> wash_runtime::wasmtime::Result<Option<BoundContainer>>
where
    T: 'static,
{
    // Binding resolution lands with the plugin's connection-snapshot wiring.
    Ok(None)
}
