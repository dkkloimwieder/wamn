# Components

A component is compiled behavior with one digest, import set, and export set.
A package owns schema contributions, operations, SQL, and contracts.
A palette component supplies behavior for wiring. The platform seals its ports, parameters, connections and digest in `palette.json`. Release and dev resolution share that reader. Palette calls use the host invocation entry and receive no public HTTP routes.
The sealed connection declaration names the node parameter that selects an application binding.
Blob nodes use `store_alias`. HTTP nodes use `requirement`.
The host reads that parameter from the pinned node configuration and checks the application's declaration and binding.
The platform owns the component. The workflow application owns the connection authority.
Components contain no deployment endpoint, database identity, or credential.

## Grouping and publication

`wamn-publish` owns package admission, artifact construction, and package and release publication.
Package coordinates are immutable. Dev publication keeps its first publication time on repeat pushes.
Release manifests carry no publication time, so their identity depends only on release content.

The default is one component per package, exporting its declared operation set.
A different import or connection requirement can justify a separate component.
A heavy dependency that must stay out of the hot execution path can also justify separation.
CRUD, command, projection, and team labels do not justify separate artifacts by themselves.

Grouping is explicit in the operation's `component` declaration.
Component names are package-local `snake_case` identifiers.
An empty name refuses, and duplicate connection requirement sets refuse.
Package operations version together even when their implementations occupy separate components.

The compiler writes final component declarations in `publication/components.json` of the build output.
Each declaration carries inline input and output schemas from the compiled operation description.
Its target names the package and version. Admission adds the environment coordinate once.
Publication readers do not merge authored declarations or resolve schema references.
A component declaration records its operations, ports, effects, static SQL, and connection requirements.
Admission derives effects from the actual imports and compares the declared operation facts.
The component's complete admitted facts remain separate from its compiled bytes.
After native resolution, the host replaces each application's linker with one built from its declared imports.
It then binds declared capabilities before preparing dispatch, including components that match no native plugin interface.
An undeclared import refuses at instantiation, and the error names that import.
Platform workloads and the separate HTTP route component use their native linkers.
Synchronous palette nodes export `wamn:node/handler`.
Asynchronous palette nodes export `wamn:node/async-handler@0.1.0`.
Application operations use their package interfaces and can await asynchronous capabilities.
The P3 HTTP shell separately exports `wasi:http/handler@0.3.0`.
Production component I/O uses native async calls for HTTP, PostgreSQL, typed operation invocation, and event settlement.
HTTP authentication and durable queue polling also use native async calls.

Pure transformations and reads of local metadata remain synchronous.
Transaction destruction closes unfinished connections without waiting for network I/O.
Explicit commit and rollback calls remain asynchronous and return clean connections to the pool.
The [capability rules](capabilities.md) govern imported authority, and [naming](naming.md) defines exported operation tokens.

The receipt operation uses typed WIT calls between Acme and Receiving.
Receiving and Acme each expose a typed purchase-order update interface in their own package.
The application model generates these WIT contracts, Rust bindings, and typed validation.
Receipt and update share generation for envelope bounds, correlation tests, exact integer encoding, and declared error details.
Update inputs distinguish an omitted field, explicit null, and a value.
The application still decides whether a field accepts null.
Receiving permits `supplier_id`; Acme permits `acme_inspection_required` and `acme_quality_status`.
Each update uses its existing SQL and permissions. Acme's update does not call Receiving's update.
Every application operation exports typed `run`. The host converts HTTP JSON using that export's WIT type.
Admission refuses resource handles in this operation boundary because those handles belong to one store.

An overlay package pins each base by digest in `base_dependencies`.
The overlay build composes each pinned base whose operations the overlay imports into the overlay component, and admission refuses a composed base that the package does not pin under that digest.
A composed component also exports the interfaces of its members. The host routes only to declared operations.
Each export declares its external connections. The component connection list is derived from its operations. An undeclared connection does not resolve for the call.
Each export carries its own statements, and the host begins a transaction for an export whose admitted operation declares one ([data access](data-access.md#inputs-queries-and-transactions)).
Each member that imports `wamn:postgres/statements` gets its own outer import, named by its position: `sql-0`, `sql-1`.
Each import is labelled as an instance of the one statements interface, so policy admits it by that interface.
The composer emits a separate instance type for each import, so each member retains its resource ownership.
A custom section of the composed component, `wamn:statement-instances`, records the digest of the member that each import was made for.
Admission reads the instantiation graph and refuses the component unless each import reaches exactly that member.
Admission copies that record into the admitted descriptor as `statement-instances`, and the imports fingerprint covers it.
Publish writes each member's own statement set into the served component, keyed by import name: the overlay's from the component's own operations, and each base's from its admitted descriptor.
The host binds each import from that served set only, and it refuses to load a composition whose release has no set for it.
Base membership alone does not authorize an operation. The [execution rules](execution.md) test the entry's own grant.

The serving manifest reads each export from its component descriptor.
It stores no second operation map.
Release membership names package versions and component digests.
Publication uses WAMN-owned OCI media types and retains the component configuration needed to establish admission.
The host compares bytes and artifact layout with the admitted facts before native loading.
A generic component pull cannot discard that configuration and still satisfy the publication contract.

## Application HTTP invocation

Publication selects the admitted application and verifies the pinned HTTP route component.
It publishes the route component separately from application bytes.
The existing `composed-entry` record identifies that route artifact and its application members.
The host loads each admitted application from its own artifact.
Overlays retain their build-time composition with their bases and their statement labels.

Publication supplies the route table through `wasi:config`, under `wamn.http.routes`.
The table includes paths, methods, accepted credentials, read limits, cache rules and the release digest.
The route component matches each method and path exactly.
It calls `wamn:operation.invoke` with JSON, and the host calls the application's typed `run` export.
The two components use separate stores, so the application call does not re-enter the route component.
Generated typed exports validate input before calling an application override.
The host retains the WAL position with the read authority.

Platform endpoints use native ingress handlers under `/wamn_control/`.
They keep their authentication, CSRF checks, operation grants and body limits.
Control runs no HTTP guest.
The composer refuses application imports of `wamn:operation` and application exports of `wasi:http/handler`.


## Guest build identity

`apps/Cargo.toml` owns guest membership and feature resolution.
The build tool selects guests from Cargo metadata and application declarations.
Each guest receives its own Cargo invocation.
Combining unrelated guests can change dependency features and therefore compiled bytes.

Standard-library guests target `wasm32-wasip2`.
The build preserves their WASI imports, and admission applies the ordinary capability policy.
The host supplies an empty application environment. Guest bytes are never rewritten.
The palette and Modbus guests use standard WASI at their existing paths.
Shared crates can live outside the guest directory.
Each dependency inherits its governed identity from its workspace.
Directory containment does not define dependency ownership.
The operations pages own build commands and artifact comparisons.

## Shared label renderer

The label renderer is an effect-free transform from fields to ZPL printer text.
`template_id` is a wiring parameter, not a caller input field.
The closed template set is `packaging`, `location`, and `product`.
An unknown template refuses during declaration validation or rendering.

The shared [label template module](../../apps/platform/no-std/label-render/src/template.rs) emits `^PW812`, `^LL1218`, and `^MD0`.
This means 812 by 1,218 dots for provisional 4-by-6-inch labels at 203 dpi.
The geometry is provisional.
There is no public interface for template authoring.

The separate blob writer uses the [blobstore capability](capabilities.md#object-storage) after rendering.
It preserves a caller's `request_id` and does not weaken object or binding authority.
A printed label, stored label, and committed application mutation remain separate outcomes.
