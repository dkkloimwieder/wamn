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

use wash_runtime::engine::ctx::{ActiveCtx, SharedCtx};
use wash_runtime::wasmtime::component::{Accessor, Resource};

use super::bindings::wasmcloud::blobstore::container::{Container, HostContainer, HostContainerWithStore};
use super::bindings::wasmcloud::blobstore::blobstore::HostWithStore;
use super::bindings::wasmcloud::blobstore::types::{
    ContainerMetadata, Error as WitError, ObjectId, ObjectMetadata,
};
use super::drain::drain_body;
use super::intake::MAX_OBJECT_BYTES;
use super::plugin::{WAMN_BLOBSTORE_ID, WamnBlobstore};
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

// The marker traits attach to the store's DATA type (`ActiveCtx`), while the
// concurrent `*WithStore` traits attach to the `HasData` type (`SharedCtx`).
// The split is the generated bindings', not ours.
impl super::bindings::wasmcloud::blobstore::types::Host for ActiveCtx<'_> {}
impl super::bindings::wasmcloud::blobstore::container::Host for ActiveCtx<'_> {}
impl super::bindings::wasmcloud::blobstore::blobstore::Host for ActiveCtx<'_> {}

impl HostContainer for ActiveCtx<'_> {
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
        let body = match instrumented(accessor, "get-data", container.get(&name)).await? {
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
        Ok(instrumented(accessor, "write-data", container.put(&name, body))
            .await?
            .map_err(|error| to_wit(&error)))
    }

    async fn list_objects(
        accessor: &Accessor<T, Self>,
        handle: Resource<Container>,
    ) -> wash_runtime::wasmtime::Result<Result<wash_runtime::wasmtime::component::StreamReader<String>, WitError>> {
        let container = container_of(accessor, &handle)?;
        let keys = match instrumented(accessor, "list-objects", container.list()).await? {
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
        Ok(instrumented(accessor, "delete-object", container.delete(&name))
            .await?
            .map_err(|error| to_wit(&error)))
    }

    async fn delete_objects(
        accessor: &Accessor<T, Self>,
        handle: Resource<Container>,
        names: Vec<String>,
    ) -> wash_runtime::wasmtime::Result<Result<(), WitError>> {
        let container = container_of(accessor, &handle)?;
        // ONE span for the batch, not one per key: the guest asked for one
        // effect. Each key is still resolved and refused on its own inside it,
        // so a containment breach cannot ride in behind valid siblings.
        let batch = async {
            for name in &names {
                container.delete(name).await?;
            }
            Ok(())
        };
        Ok(instrumented(accessor, "delete-objects", batch)
            .await?
            .map_err(|error| to_wit(&error)))
    }

    async fn has_object(
        accessor: &Accessor<T, Self>,
        handle: Resource<Container>,
        name: String,
    ) -> wash_runtime::wasmtime::Result<Result<bool, WitError>> {
        let container = container_of(accessor, &handle)?;
        Ok(instrumented(accessor, "has-object", container.has(&name))
            .await?
            .map_err(|error| to_wit(&error)))
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
        match instrumented(accessor, "object-info", container.head(&name)).await? {
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
        Ok(instrumented(accessor, "clear", container.clear())
            .await?
            .map_err(|error| to_wit(&error)))
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

    /// `name` is the guest's own declared STORE ALIAS, not a container name.
    ///
    /// The container is environment-owned, so a guest that had to name it
    /// would first need the coordinate the confinement exists to keep from it.
    /// Naming its own alias is the only thing a component can honestly do
    /// here, and the resolved binding supplies the real container.
    async fn get_container(
        accessor: &Accessor<T, Self>,
        name: String,
    ) -> wash_runtime::wasmtime::Result<Result<Resource<Container>, WitError>> {
        // Every refusal reports `no-such-container`. Distinguishing "unbound"
        // from "bound but unauthorized" would tell a guest which aliases
        // exist, which is the leak `container-exists` is also shaped to avoid.
        let Ok(container) = resolve_alias(accessor, &name).await else {
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
        Ok(Ok(resolve_alias(accessor, &name).await.is_ok()))
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

/// The plugin and the calling component, for instrumentation and resolution.
fn plugin_and_caller<T>(
    accessor: &Accessor<T, SharedCtx>,
) -> wash_runtime::wasmtime::Result<(std::sync::Arc<WamnBlobstore>, String)>
where
    T: 'static,
{
    accessor.with(|mut access| {
        let ctx = access.get();
        let component_id = ctx.component_id.to_string();
        ctx.try_get_plugin::<WamnBlobstore>(WAMN_BLOBSTORE_ID)
            .map(|plugin| (plugin, component_id))
    })
}

/// Run one store effect inside its span and record its latency.
///
/// Every store touch goes through here, so an effect cannot be added later
/// that is invisible to a trace.
async fn instrumented<T, F, R>(
    accessor: &Accessor<T, SharedCtx>,
    operation: &'static str,
    effect: F,
) -> wash_runtime::wasmtime::Result<Result<R, StoreError>>
where
    T: 'static,
    F: std::future::Future<Output = Result<R, StoreError>>,
{
    use tracing::Instrument as _;

    let (plugin, component_id) = plugin_and_caller(accessor)?;
    let span = super::plugin::blobstore_span(&plugin, &component_id, operation);
    let started = std::time::Instant::now();
    let outcome = effect.instrument(span).await;
    crate::plugins::effect_span::record_effect_ms(
        &crate::plugins::effect_span::BLOBSTORE_DURATION_MS,
        crate::plugins::effect_span::EFFECT_OPERATION,
        operation,
        &plugin.project,
        started.elapsed(),
    );
    if let Err(error) = &outcome {
        tracing::warn!(effect.operation = operation, error = %error, "blobstore effect refused");
    }
    Ok(outcome)
}

/// Resolve one store alias into its confined container, live.
///
/// Reaches the plugin through the store context the way `connection_http`
/// does, then goes through the shared authority reader.
async fn resolve_alias<T>(
    accessor: &Accessor<T, SharedCtx>,
    store_alias: &str,
) -> Result<BoundContainer, super::binding::BindingError>
where
    T: 'static,
{
    let resolved = accessor.with(|mut access| {
        let ctx = access.get();
        let component_id = ctx.component_id.to_string();
        ctx.try_get_plugin::<WamnBlobstore>(WAMN_BLOBSTORE_ID)
            .map(|plugin| (plugin, component_id))
    });
    let (plugin, component_id) = match resolved {
        Ok(resolved) => resolved,
        Err(error) => {
            tracing::warn!(error = %error, "blobstore plugin is not registered on this host");
            return Err(super::binding::BindingError::Unauthorized);
        }
    };
    plugin.container_for(&component_id, store_alias).await
}
