# Components

A component is compiled behavior with one digest, import set, and export set.
A package owns schema contributions, operations, SQL, and contracts.
A palette component supplies schema-less behavior for wiring and has its own component identity.
Components contain no deployment endpoint, database identity, or credential.

## Grouping and publication

The default is one component per package, exporting its declared operation set.
A different import or connection requirement can justify a separate component.
A heavy dependency that must stay out of the hot execution path can also justify separation.
CRUD, command, projection, and team labels do not justify separate artifacts by themselves.

Grouping is explicit in the operation's `component` declaration.
Component names are package-local `snake_case` identifiers.
An empty name refuses, and duplicate connection requirement sets refuse.
Package operations version together even when their implementations occupy separate components.

A component declaration records its operations, ports, effects, static SQL, and connection requirements.
Admission derives effects from the actual imports and compares the declared operation facts.
The component's complete admitted facts remain separate from its compiled bytes.
Synchronous nodes export `wamn:node/handler`.
Nodes that await asynchronous capabilities export `wamn:node/async-handler@0.1.0` with an asynchronous lift.
The P3 HTTP shell separately exports `wasi:http/handler@0.3.0`.
The [capability rules](capabilities.md) govern imported authority, and [naming](naming.md) defines exported operation tokens.

Application operation dependencies resolve to exact admitted providers.
Every interface imported by the admitted closure has one provider, including its interface version.
Repeated export-only handlers do not create that uniqueness requirement.
Dependency membership alone does not authorize a nested operation.
The [execution rules](execution.md) retain the original caller and each operation's authority.

Release membership names package versions and component digests.
Publication uses WAMN-owned OCI media types and retains the component configuration needed to establish admission.
The host compares bytes and artifact layout with the admitted facts before native loading.
A generic component pull cannot discard that configuration and still satisfy the publication contract.

## Guest build identity

`apps/Cargo.toml` and `apps/platform/no-std/Cargo.toml` own guest membership and feature resolution.
The build tool selects guests from Cargo metadata and application declarations.
Each guest receives its own Cargo invocation.
Combining unrelated guests can change dependency features and therefore compiled bytes.

Standard-library guests target `wasm32-wasip2` and pass through the pinned WASI virtualizer before admission.
Its fixed profile supplies an empty environment, denies stdio and exit, passes clocks, and removes unused virtualization.
It does not run `wasm-opt`.
The resulting component must satisfy the ordinary import policy without an exemption.
Existing `no_std` guests keep their separate workspace.

The virtualizer can change imported WASI versions.
Its revision and adapter digest must agree with the exact versions in the [capability declaration](capabilities.md#import-admission).
The [native alignment owner](native-alignment.md) records why this build step remains.
The operations pages own build commands and artifact comparisons.

## Shared label renderer

The label renderer is an effect-free transform from fields to ZPL printer text.
`template_id` is a wiring parameter, not a caller input field.
The closed template set is `pallet`, `location`, and `product`.
An unknown template refuses during declaration validation or rendering.

The shared [label template library](../../apps/platform/no-std/label-template/src/lib.rs) emits `^PW812`, `^LL1218`, and `^MD0`.
This means 812 by 1,218 dots for provisional 4-by-6-inch labels at 203 dpi.
The geometry remains provisional until a real printer requirement replaces it.
Template authoring is not an existing public interface.

The separate blob writer uses the [blobstore capability](capabilities.md#object-storage) after rendering.
It preserves a caller's `request_id` and does not weaken object or binding authority.
A printed label, stored label, and committed application mutation remain separate outcomes.
