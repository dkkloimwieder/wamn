# Integration and connection schema

## Model

Integrations are configured per project, connections per project environment, and an application uses a connection through a release-scoped binding.

- **integration\_type** is the platform catalog of integration code (`http`, `s3`, `gcs`, `disk`, `modbus`, `netsuite_rest`, …). Every build or parameter-shape change is an **integration\_type\_version**.
- **auth\_type** is a seeded platform catalog: exactly the schemes the host implements (`http_basic`, `api_key`, `oauth2_client_credentials`, `oauth2_authorization_code`, `aws_sigv4`, `custom`, `none`). Which auth types a type supports is a bridge table.
- **integration** is one project's named instance of a type: name, type, auth type, optional external system. It holds no parameters. Different credentials against the same external account make a new integration of the same type.
- **connection** is that integration in one project environment: all its parameters, its credential and its service principal, as immutable generations. Exactly one per integration per env.
- **binding** maps a name an application declares in `wamn.k` to a connection, per release. Several components and applications may share one connection. `environment.k` apply writes bindings; nobody creates them by hand. No binding, no use for component calls; host-managed file access (`file.get`) reads a file through its own connection under the file permissions.
- **external\_system** is identity only: one external account that records come from (one NetSuite account). It has no type, parameters or credentials. Integrations that reach it set `integration.external_system_id`; most (compute, storage, analytics) leave it NULL. Provenance keys on `external_system_id` + `external_id`. Example: `netsuite_rest`, `netsuite_restlet` and `netsuite_odbc` are three types (they differ in form, interface and auth), configured as three integrations sharing one external system.
- No allow-policy table. Configuring and binding are the project admin's job.
- Names are identity and never change.

Why per project, not per org env: smaller credential blast radius, external-side least privilege per project, cost attribution per project, the project admin owns it end to end, and a project copy or retire is self-contained. The cost is repeated configuration across projects; a later "copy from project" action covers it with no schema change.

Not covered here: what a forked database (prod copied to dev) does with rows that point at the source environment, such as `platform.file.connection_id`. Other pieces need the same rule.

## Tables (system DB)

Ten tables, every reference a real FK. Platform-owned catalogs first, then org data.

```sql
-- catalogs (platform-owned)
CREATE TABLE registry.integration_type (
    code      text PRIMARY KEY CHECK (code ~ '^[a-z][a-z0-9_-]*$'),
    form      text NOT NULL CHECK (form IN ('host', 'wasm', 'native')),
    exclusive boolean NOT NULL,                      -- one physical resource, one holder at a time
    resource_key_template text CHECK (resource_key_template <> ''),  -- e.g. serial:{device}; uses immutable fields only;
                                                     -- no version may change it (publish check)
    CHECK (exclusive = (resource_key_template IS NOT NULL))
);

CREATE TABLE registry.integration_type_version (    -- every build, every parameter-shape change
    integration_type text NOT NULL REFERENCES registry.integration_type (code),
    version          integer NOT NULL CHECK (version > 0),
    digest           text CHECK (digest ~ '^sha256:[0-9a-f]{64}$'),  -- the bytes; NULL only for host types (publish check); not unique
    parameter_schema jsonb NOT NULL,                 -- flat object; per field: optional immutable, optional deprecated
    operations       text NOT NULL,                  -- the one exported WIT world, e.g. wamn:modbus/client@1.0.0
    published_at     timestamptz NOT NULL,
    PRIMARY KEY (integration_type, version)
);

CREATE TABLE registry.auth_type (
    code              text PRIMARY KEY,
    credential_schema jsonb NOT NULL,                -- fields the secret must carry
    parameter_schema  jsonb NOT NULL,                -- placement: api_key header or query name, OAuth token URL and scopes
    http_only         boolean NOT NULL               -- paired only with HTTP-interface types (checked at every version publish)
);

CREATE TABLE registry.integration_type_auth (
    integration_type text NOT NULL REFERENCES registry.integration_type (code),
    auth_type        text NOT NULL REFERENCES registry.auth_type (code),
    PRIMARY KEY (integration_type, auth_type)
);

-- org data
CREATE TABLE registry.external_system (                -- identity only
    id      uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    org     text NOT NULL,
    project text NOT NULL,
    name    text NOT NULL CHECK (name ~ '^[a-z][a-z0-9_-]{0,62}$'),  -- e.g. netsuite-acme
    UNIQUE (org, project, name),
    UNIQUE (id, org, project),
    FOREIGN KEY (org, project) REFERENCES registry.projects (org, id)
);

CREATE TABLE registry.integration (
    id                 uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    org                text NOT NULL,
    project            text NOT NULL,
    integration_type   text NOT NULL,
    auth_type          text NOT NULL,
    external_system_id uuid,                         -- NULL unless the integration is a record source
    name               text NOT NULL CHECK (name ~ '^[a-z][a-z0-9_-]{0,62}$'),
    status             text NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'disabled')),
    UNIQUE (org, project, name),
    UNIQUE (id, org, project, integration_type),
    FOREIGN KEY (org, project) REFERENCES registry.projects (org, id),
    FOREIGN KEY (external_system_id, org, project)
        REFERENCES registry.external_system (id, org, project),
    FOREIGN KEY (integration_type, auth_type)
        REFERENCES registry.integration_type_auth (integration_type, auth_type)
);

-- project_envs.target_id is nullable on main (system-schema.sql registry.project_envs): an env needs its
-- target set before a connection can be created in it
ALTER TABLE registry.project_envs ADD UNIQUE (org, project, env, target_id);

CREATE TABLE registry.connection (                     -- the integration in one env
    id                   uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    integration_id       uuid NOT NULL,
    org                  text NOT NULL,
    project              text NOT NULL,
    env                  text NOT NULL,
    integration_type     text NOT NULL,                  -- projected, so the env database can check types
    target_id            uuid NOT NULL,                  -- where the env runs: the cloud cluster or a local box
    resource_key         text,                           -- NOT NULL iff the type is exclusive (checked at write)
    active_generation    bigint NOT NULL,
    service_principal_id uuid NOT NULL UNIQUE,
    principal_type       text NOT NULL DEFAULT 'service' CHECK (principal_type = 'service'),
    status               text NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'disabled')),  -- admin only
    UNIQUE (integration_id, env),
    FOREIGN KEY (integration_id, org, project, integration_type)
        REFERENCES registry.integration (id, org, project, integration_type),
    FOREIGN KEY (org, project, env, target_id)
        REFERENCES registry.project_envs (org, project, env, target_id),
    FOREIGN KEY (service_principal_id, principal_type)
        REFERENCES identity.principals (id, type)
);
CREATE UNIQUE INDEX connection_resource_per_target
    ON registry.connection (target_id, resource_key)
    WHERE resource_key IS NOT NULL AND status = 'active';     -- disabling frees the resource

CREATE TABLE identity.credential (                     -- a reference, never the value; one immutable row per rotation
    id            uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    connection_id uuid NOT NULL REFERENCES registry.connection (id) ON DELETE CASCADE,
    store         text NOT NULL CHECK (store IN ('k8s', 'gcp_sm', 'file')),
    ref           text NOT NULL,                         -- secret name or resource path
    version       text NOT NULL,                         -- the store's version id
    hint          text,                                  -- last 4 characters, for display
    created_at    timestamptz NOT NULL,
    created_by    uuid NOT NULL,
    UNIQUE (id, connection_id)
);

CREATE TABLE identity.oauth_token (                    -- host-only; both OAuth types, shared across pods
    connection_id  uuid PRIMARY KEY REFERENCES registry.connection (id) ON DELETE CASCADE,
    ciphertext     bytea NOT NULL,                       -- AES-256-GCM: access token; refresh token for authorization code
    nonce          bytea NOT NULL,
    key_id         text NOT NULL,                        -- envelope key in Cloud KMS (local: key file)
    expires_at     timestamptz NOT NULL,
    refresh_status text NOT NULL DEFAULT 'ok' CHECK (refresh_status IN ('ok', 'failing')),
    updated_at     timestamptz NOT NULL                  -- fetch and refresh lock the connection row, not this one
);
-- authorization code: no row = not authorized; calls refused until connection.authorize completes
-- client credentials: no row = no token yet; the first call fetches one with the client secret
-- every fetch and refresh: FOR NO KEY UPDATE on the registry.connection row (it always exists), re-read this row,
-- call the provider only if the token is missing or due; connection.write and set_credential lock the same row
-- with a new generation, in the same transaction:
--   client credentials: delete this row when the credential or parameters.auth changes
--   authorization code: delete this row when parameters.auth changes (authorize again);
--                       set expires_at = '-infinity' when only the secret changes (the refresh token stays)

CREATE TABLE registry.connection_generation (          -- immutable (trigger, as on main)
    connection_id uuid NOT NULL REFERENCES registry.connection (id) ON DELETE CASCADE,
    generation    bigint NOT NULL CHECK (generation > 0),
    parameters    jsonb NOT NULL DEFAULT '{}',           -- all parameters for this env; auth placement under "auth"
    credential_id uuid,                                  -- NULL only for auth type none (checked at write)
    created_at    timestamptz NOT NULL,
    created_by    uuid NOT NULL,
    PRIMARY KEY (connection_id, generation),
    FOREIGN KEY (credential_id, connection_id)
        REFERENCES identity.credential (id, connection_id)   -- the credential belongs to this connection
);
ALTER TABLE registry.connection ADD FOREIGN KEY (id, active_generation)
    REFERENCES registry.connection_generation (connection_id, generation) DEFERRABLE INITIALLY DEFERRED;
-- immutability (connection_generation, credential): UPDATE always raises 55000; DELETE raises 55000
-- unless the parent registry.connection row is already gone (the cascade from connection.delete)
-- registry.connection.active_generation only increases (trigger): configuration never moves back
```

- The first `connection.write` carries the credential (unless the auth type is `none`), so the first generation is complete. The generation number is taken under `SELECT … FOR UPDATE` on the connection row.
- A generation's credential belongs to its connection, and the principal is a `service` principal: both are composite FKs.
- A connection serves only when its integration and itself are both `active`.

## Project env DB

Each project environment database holds read-only copies of its own `external_system`, `integration` and `connection` rows, plus its own catalog tables. The copies are pushed by the control host; the push is not atomic with the system database, so `env apply` re-syncs them synchronously at expand, under its lock, before it writes `release_connection`.

```sql
-- projection (read-only)
CREATE TABLE platform.external_system (
    id   uuid PRIMARY KEY,
    name text NOT NULL UNIQUE
);
CREATE TABLE platform.integration (
    id                 uuid PRIMARY KEY,
    integration_type   text NOT NULL,
    external_system_id uuid REFERENCES platform.external_system (id),
    name               text NOT NULL UNIQUE,
    status             text NOT NULL
);
CREATE TABLE platform.connection (
    id               uuid PRIMARY KEY,
    integration_id   uuid NOT NULL UNIQUE REFERENCES platform.integration (id),  -- one per env
    integration_type text NOT NULL,
    status           text NOT NULL,       -- effective: active only if the integration and the connection are active
    UNIQUE (id, integration_type)
);

-- catalog: what a component declares, materialized at package install (today catalog.connection_requirements)
CREATE TABLE catalog.connection_requirement (
    env_key          text NOT NULL,
    component_digest text NOT NULL CHECK (component_digest ~ '^sha256:[0-9a-f]{64}$'),
    package_id       text NOT NULL,          -- name, type and mode come from the package's wamn.k
    name             text NOT NULL,          -- declared in wamn.k
    integration_type text NOT NULL,
    interfaces       text[] NOT NULL CHECK (cardinality(interfaces) > 0),  -- every interface it imports for the name, from its WIT
    mode             text NOT NULL CHECK (mode IN ('read', 'write')),
    PRIMARY KEY (env_key, component_digest, package_id, name),
    UNIQUE (env_key, component_digest, package_id, name, integration_type)
);

-- catalog: the version one release uses of one connection
CREATE TABLE catalog.release_connection (
    env_key          text NOT NULL,
    manifest_digest  text NOT NULL,          -- the release
    connection_id    uuid NOT NULL,
    integration_type text NOT NULL,
    version          integer NOT NULL,       -- from environment.k; validated at apply against the system catalog
    PRIMARY KEY (env_key, manifest_digest, connection_id),
    UNIQUE (env_key, manifest_digest, connection_id, integration_type),
    FOREIGN KEY (env_key, manifest_digest) REFERENCES catalog.releases (env_key, manifest_digest),
    FOREIGN KEY (connection_id, integration_type) REFERENCES platform.connection (id, integration_type)
);

-- catalog: name to connection, per release (today catalog.connection_bindings)
CREATE TABLE catalog.connection_binding (
    env_key          text NOT NULL,
    manifest_digest  text NOT NULL,
    component_digest text NOT NULL,
    package_id       text NOT NULL,
    name             text NOT NULL,
    connection_id    uuid NOT NULL,
    integration_type text NOT NULL,          -- in every FK: a type mismatch is a database error
    PRIMARY KEY (env_key, manifest_digest, component_digest, package_id, name),
    FOREIGN KEY (env_key, manifest_digest, connection_id, integration_type)
        REFERENCES catalog.release_connection (env_key, manifest_digest, connection_id, integration_type),
    FOREIGN KEY (env_key, component_digest, package_id, name, integration_type)
        REFERENCES catalog.connection_requirement (env_key, component_digest, package_id, name, integration_type)
);
```

- Admission is per component: the host refuses a call on a name the calling component did not declare.
- Record provenance: an application model that imports records carries `external_system_id` (FK `platform.external_system (id)`) and `external_id`, with `UNIQUE (external_system_id, external_id)`.
- `platform.file.connection_id` points at `platform.connection`. A forked database keeps prod's rows; provenance pointing at prod's external system is correct, because the records came from it.

## Deployment

Releases coexist during a rollout and drain per release digest; each release reads its own `release_connection` and binding rows.

- Authority: integrations, connections and generations live in the system database; the project database holds read-only copies.
- Connection lifecycle is administration, not deployment. Apply never creates, retires or disables a connection; it only adds and removes `release_connection` and binding rows. An unbound connection is normal.
- Connection definitions (type, parameters, credential) are written through the UI and API, never in `environment.k`. `environment.k` is the one authority for bindings and versions; the UI shows them read-only.
- Expand before the rollout: the target release's `release_connection` and binding rows exist before the Helm write. Contract after the drain: those rows of releases outside the live set are removed. They are a projection of the last applied document, per release only so releases can coexist during a rollout; they are not history.
- A connection-only apply writes no Helm revision, so `release_connection` and `connection_binding` carry record history (stamp and log triggers).
- The version belongs to a release's use of a connection. In steady state an env has one live release, so one version per connection; during a rollout the old release keeps its version and the new one uses its own. The host loads both builds side by side (it caches `InstancePre` per digest).
- The live release's versions and bindings can change in place. A compatible version change updates its `release_connection.version`: live, no rollout. A breaking change ships with a new application release.
- Runtime refresh: every change to a generation, `release_connection` or `connection_binding` publishes an invalidation event. The host caches (release, component, package, name) to (connection, version, generation) and converges within 60 s at worst. Calls in flight finish on what they started with; the host loads a new version's artifact and builds its `InstancePre` before switching.
- A live version change may cross a running workflow run. Integration code holds no state between calls; any token it returns (a cursor, a page token) is the external system's.

| Change | Mechanism | Rollout | Application rebuilt |
| --- | --- | --- | --- |
| Credentials, parameters | new generation (UI or API) | no | no |
| Compatible version change | `release_connection.version` of the live release (`environment.k` apply) | no | no |
| Binding for the live release | binding rows (`environment.k` apply) | no | no |
| Breaking version change | new application release with its own `release_connection` | yes | yes |
| New application release | expand, Helm rollout, drain, contract | yes | yes |

### Rollback

Configuration is never rolled back. Credentials, parameters, versions and bindings only move forward; a fix is a new edit or a new apply. Generations are an audit record, not rollback points. Rollback rolls back code only:

1. **No breaking integration change between the releases:** `env rollback --reason` applies the document with the previous deployed digest and every other field as the authorities hold it now.
2. **A breaking change between them** (release A needs v4, release B needs an incompatible v5): `env rollback` refuses, and so does changing v5 to v4 while B runs. The one valid path is a single apply of a document naming A and v4 together: apply validates the pair, expands A's rows with v4, rolls out A, drains B (which keeps v5 until its pods exit), and contracts. `env rollback` names this path in its refusal. It needs v4 and v5 to share the connection's one generation during the drain, which the field lifecycle guarantees for adjacent versions; otherwise apply refuses, and the way back is a second integration for A or stepping through the intermediate version.

## Exclusive resources

A resource is a physical thing one holder may use at a time on a target, such as a serial port or a Modbus device. A target is where an env runs: the cloud cluster or a local box.

- Identity: `integration_type.resource_key_template` (`serial:{device}`, `tcp:{host}:{port}`), rendered once from immutable fields into `connection.resource_key`, stable across versions and releases.
- Static check: the unique index on active connections per (target, resource\_key). Disabling a connection frees the resource; re-enabling it is refused while an active replacement holds the same key.
- Runtime lease: a Postgres session advisory lock in the system database (two envs may share a target, and advisory locks are per database), taken on a dedicated session on a 64-bit `hashtextextended` of (target, resource\_key). The holder is the host process, which opens the resource once and serializes calls from every component bound to it, so several components (for example a reader and a writer) may share one exclusive connection. A crashed or killed pod drops its session and the lock. On a local box the lease is in-process, because one host serves every env there.
- One replica: an env that binds an exclusive connection runs one host replica. Replicas share the run queue (fact: `run-queue.sql`), so a second replica would claim runs it cannot serve while the first holds the lease. Apply reads the count from the host group body it takes from `WAMN_RELEASE_HOST_GROUP` (fact: `environment/mod.rs` `host_group`, `release_chart.rs`); with an exclusive binding it refuses unless that body names `replicas: 1`, and it never falls back to the chart default. No per-call locking and no routing to the holder.
- During a rollout the new pod waits for the lease until the draining pod exits. A pod's readiness never depends on acquiring a lease, so the wait cannot block the rollout.
- Disabling a connection releases the lease: on the invalidation event each host stops new calls, lets calls in flight finish, closes the resource and drops the lock. A replacement waits for the lock; it never takes it over.

## Read and write

Three separate controls; none is a column on the connection or the binding.

- **Which users may write:** the ACL on application operations. A user without the operation is refused before any connection is touched.
- **What the code may do:** `connection_requirement.mode`, declared in `wamn.k`. Two values; `write` includes `read`.
  - wasm and native types: WIT enforces it. The version's one world exports a read interface and a write interface; apply checks that a `read` requirement lists only read interfaces in `interfaces`. A `write` requirement may import both, or write alone.
  - host types: one interface serves every mode (HTTP: `wamn:connection/http`), so the host enforces mode at run time, as main does today (fact: `connection_http.rs::authorize_method` allows only GET and HEAD on a read binding; `wamn_blobstore/store.rs::with_read_only`). The interface check does not apply.
- **What the connection can do at all:** the credential, such as a read-only IAM or ERP role, enforced by the external system.

```wit
package wamn:modbus@1.0.0;
interface read  { read-registers: func(addr: u16, count: u16) -> result<list<u16>, error>; }
interface write { write-register: func(addr: u16, value: u16) -> result<_, error>; }
world client {
  export read;
  export write;
  export wamn:integration/health@0.1.0;
}
// mode = "read": imports wamn:modbus/read only, so write-register does not exist for it
// mode = "write": imports both
```

## Parameters

Every parameter is connection-scoped, set per env on the connection generation. A change in one env never reaches another. The copy action copies parameters from another env as a new generation in the target env; it never copies credentials.

- An edit appends a generation and makes it active; every bound release sees it on its next request. No deployment, no restart.
- Validation is wamn's own. The schema is a flat object, with custom keywords on top-level properties only. Values must validate against every version a live release uses of the connection (both, during a rollout). Fields declared only by other published versions are accepted as staged; fields unknown to every published version are refused. Per-version `additionalProperties: false` is not used, because staging validates against the union.
- Auth placement lives under `parameters.auth`, validated against the auth type's `parameter_schema`; a publish check forbids a top-level `auth` property in any type schema.
- Fields marked `immutable` (storage location: bucket, base path, endpoint host) cannot change after creation. Moving storage is a new integration.
- Retained rollback releases are not checked at write; `env rollback` re-validates and refuses if the values no longer fit.

### Field lifecycle (expand, deprecate, contract)

A field changes shape only through a new name, over three versions. `deprecated` sits beside `immutable` on the field; the UI shows a deprecated field struck through and API reads flag it.

| Version | Old field `realm` | New field `account_realm` |
| --- | --- | --- |
| v4 | required | — |
| v5 | deprecated, optional | optional |
| v6 | removed | required |

- Checked at publish: a field may be removed only if the previous version deprecated it; may become required only if the previous version had it as optional; its JSON type and meaning never change. Adjacent versions (N, N+1) can therefore always share one set of stored values.
- Checked at apply: moving to v6 refuses until the new field has a value. Any versions may be in use at once, provided the stored values validate against all of them.

## Files

- File stores are host-form types only (`gcs`, `s3`, `disk`); `platform.file.connection_id` may reference only such a connection (checked at write).
- A file is read through its own `file.connection_id` with the host's built-in store code and the connection's active generation, authorized by the application's `file.get` permission, not by a binding. Its location is fixed by immutable fields and `blob_key`, so `file.connection_generation` goes.
- A connection with live `platform.file` rows cannot be deleted; disabling it makes those files unreadable.

## Versions and WIT

WIT is the contract between application and integration code. Integration code has three contracts; only the operations contract forces an application release.

| Contract | Between | Form | Changes when | Who acts |
| --- | --- | --- | --- | --- |
| Binding | host to integration code | WIT `wamn:integration/bound` | almost never | platform release |
| Parameters | admin to integration code | data: `parameter_schema` | the code needs new configuration | publish a version, stage fields in the UI, move the version in `environment.k` |
| Operations | application to integration code | WIT world in `operations` | the code does something new | the above plus an application rebuild and release |

On main (fact): `wamn:connector@0.1.0` `bound` exposes only `authority()` and `credential()`, and `reader.get` takes and returns opaque bytes.

- The package is renamed `wamn:integration`.
- Add `parameters: func() -> string` (validated JSON) to `bound`. The host strips `auth` before returning it, because the host applies auth; only the `custom` auth type receives it, beside `bound.credential()`.
- `wamn:integration/reader` is retired. Each version exports exactly one typed WIT world (`operations`), which may hold read and write interfaces and always exports `wamn:integration/health` (`check: func() -> result<_, string>`). Host types implement health in the host.
- Types share interfaces where the protocol is shared (`s3`, `gcs` and `disk` export blobstore; HTTP-based types export `wamn:connection/http`); a unique protocol gets its own package (`wamn:modbus`, `wamn:netsuite/rest`).
- An application imports interfaces, never a type. The generator emits one typed handle per name declared in `wamn.k`.
- Within one release, every component bound to a connection imports from that release's one version, so consumers of a connection upgrade together; they may import different interfaces of its world. Apply refuses a mismatch and names the component; `wamn build` warns first.
- Compatibility is component-model semver: 1.x within a major, 0.x within a minor. Most wamn contracts are 0.x, so a minor bump forces a rebuild until they reach 1.0. Interface packages keep their versions.

## Where the files and the UI fit

The developer declares a need in `wamn.k`, the admin configures integrations and connections in the UI or API, and `environment.k` joins the two. Main has both `.k` halves (fact: `apps/netsuite/wamn.k`, `deploy/gcp/environments/dkk--wms--dev.k`).

```
# wamn.k: the name the code uses, the type and the mode
connections = {erp = {integration_type = "netsuite_rest", mode = "write"}}

# environment.k: the version each integration runs in this env, and which integration fills each name
integrations = {"acme-netsuite" = {version = 4}}
applications = {
    receiving = {packages = ["receiving"], connections = {erp = "acme-netsuite"}}
}
```

| Source | Who | Writes | Headless |
| --- | --- | --- | --- |
| `wamn.k` build and install | developer | `connection_requirement`, plus the typed handle in code | `wamn build`, install |
| create integration | admin | `integration` (name, type, auth type) | `integration.write` |
| per env parameters, credential, disable | admin | a new `connection_generation` and the secret in the store | `connection.*` routes |
| `environment.k` apply | deployment | `release_connection` and `connection_binding` | `wamn-ctl env apply` |

- Apply refuses when the integration's type differs from the declared type, when the env has no active connection for it, or when the version does not export a compatible interface.
- Changes from main: `wamn.k` renames `integration` to `integration_type`; `environment.k` names a project integration instead of an org connection, gains `integrations = {name = {version}}`, and loses `connectors = {…}`.

## Headless interface

Everything has a headless interface. Every live action is a control host route; the UI, `wamn-ctl` and scripts call the same routes, in main's `noun.verb` style (`crates/catalog/model/src/host_route.rs`). Authority moves from org admin to project admin (org admins hold it implicitly).

On main (fact): five control routes (`external_system.write`, `connection.write`, `project_connection.enable`, `project_connection.disable`, `integration_allow.allow`), all org-admin and write-only; `connection rotate` and `import-credentials` exist only as `wamn-ctl` verbs over SQL functions; `wamn-ctl bind-connection` writes a release-scoped binding; `wamn-ctl integration build` and `publish` handle wasm artifacts.

| Operation | `wamn-ctl` | On main |
| --- | --- | --- |
| `integration_type.list` (get) | `integration-type list` | new |
| `integration_type.publish` (platform) | `integration publish` | exists; now records a version with its schema and world |
| `external_system.write`, `.list`, `.delete` | `external-system …` | write exists (becomes project-scoped, identity only); list, delete new |
| `integration.write`, `.list`, `.get` | `integration …` | new; replaces org `connection.write` |
| `integration.disable`, `.enable`, `.delete` | `integration …` | new |
| `connection.write` | `connection write` | reshaped: per env, all parameters; the first write carries the credential and creates the principal |
| `connection.copy` | `connection copy` | new: parameters from another env, never credentials |
| `connection.set_credential` | `connection set-credential` | today `connection rotate` (file input); value write-only |
| `connection.authorize` | `connection authorize` | new; `oauth2_authorization_code` only |
| `connection.test` | `connection test` | new; candidate parameters and credential inline, in memory |
| `connection.disable`, `.enable`, `.delete` | `connection …` | new |
| `connection.list`, `.get` | `connection …` | new; credential hint, status, versions read-only |

Removed: `project_connection.enable` and `.disable`, `integration_allow.allow`, `connection.import_credentials`, `wamn-ctl bind-connection`.

Disable and delete:

- Disabling an integration or a connection makes it stop serving and releases its leases.
- Delete removes a row created by mistake when nothing references it; anything referenced is disabled instead. The control host holds the administration login of every env database in the org, so it checks each env database of the project under the env lifecycle lock, deletes from the system database, then removes the projected rows.
  - A connection needs no binding, no `release_connection` row and no `platform.file` row. Its generations, credential rows and token row cascade with it, and its store secrets are destroyed; its service principal is disabled, not deleted, because the write log and ACL history reference principal ids.
  - An integration needs its connections deleted first.
  - An external system needs no integration and no record pointing at it. The FK from application records to `platform.external_system` makes deleting the projected row fail, so no scan is needed. Records block only the external system, never its sibling integrations.

## Upgrades

Integration rules for the platform and application upgrade paths:

- **Application upgrade:** apply's compatibility step checks that each named version is published and served, each component's imports are compatible with it, and required fields have values. Expand re-syncs the projection, validates existing connections against the new versions and writes the new release's rows; contract removes the old release's rows.
- **The host serves versions N and N-1 of every host type.** A platform upgrade drops N-1 only when no live release uses it; otherwise it refuses and names the releases. A retained rollback release that needs a dropped version stops being a rollback target, and the upgrade says which.
- **wasm and native versions are never deleted automatically.** A retained `environment.k` in git may name any of them; pruning is a manual platform action.
- A new version appears in the catalog only; nothing moves until a document changes a version. Not now: `version = "latest-compatible"`.
- A rollback target whose version a platform upgrade dropped, or whose parameters no longer validate, is refused at apply.

| Change | Path | Rollout | Undo |
| --- | --- | --- | --- |
| Credential or parameter | UI or API, new generation | no | none: a new edit |
| Compatible version, live release | `environment.k` apply | no | apply the previous version |
| Binding, live release | `environment.k` apply | no | apply the previous document |
| Disable or enable | UI or API | no | enable again |
| Breaking version | rebuild, new release | yes | see Rollback |

## Secrets and auth

Plaintext secrets never sit in Postgres. A credential row holds a reference (`store`, `ref`, `version`), and the host resolves it through a backend trait. Today main stores plaintext in `identity.credential.secret`, which breaks the system schema's own R8b rule (fact).

| Secret | Where |
| --- | --- |
| Static secret (API key, password, service-account JSON, OAuth client secret) | Kubernetes Secret first (kind and GKE, local parity), one Secret per rotation named by the credential id, since a Secret has no version history; GCP Secret Manager next (native versions); file on local targets |
| OAuth tokens (both OAuth types) | `identity.oauth_token`, AES-256-GCM, envelope key in Cloud KMS (local: key file) |

- Old secret material is never read again, because configuration is never rolled back: it is destroyed asynchronously 5 minutes after a rotation becomes active. The credential row stays as audit metadata.
- GKE application-layer secrets encryption (Cloud KMS) must be on for the Kubernetes backend.
- No External Secrets Operator: credentials are created at run time, and one custom resource per credential is churn. The host reads the store directly.

### Auth runs in the host

- The host applies auth on the outbound call, as it handles application auth today; integration code never holds the secret, except with `custom`, which hands it over through `bound.credential()`.
- `http_basic`, `api_key`, both OAuth types and `aws_sigv4` are `http_only`. Non-HTTP types (Modbus, serial, raw TCP) use `none` or `custom`.
- Auth type sits on the integration, so every env of an integration uses the same auth; a different auth is a different integration.
- The host caches each connection's secret or token, drops it on the invalidation event, re-resolves on any 401 or 403, and holds a 60 s TTL as a backstop for a lost event. No strict emergency revocation.
- OAuth refresh runs before expiry, once across all pods: a host locks the connection row (`FOR NO KEY UPDATE`), re-reads the token row, and calls the provider only if the token is missing or due. The same lock covers the first client-credentials fetch and serializes refresh with rotation. A failed refresh sets `refresh_status` and notifies; retries continue on a schedule. The cache is warmed at startup for bound connections; the invalidation event for a new generation drops it.

### Admin flow (no deployment)

1. The admin pastes the secret into a write-only field in the UI.
2. Optional: `connection.test` runs the candidate parameters and credential in memory through `health.check()`; nothing is committed. Its optional version defaults to the live release's version, or the newest published one when nothing is bound.
3. The control host writes the secret to the store and inserts a new immutable `identity.credential` row with the store's version id.
4. A new generation pointing at that row becomes active, and the control host publishes an invalidation event.
5. There is no switching back: a wrong or leaked credential is fixed by setting a new one. The value is never shown again; the UI shows `hint`, `created_by`, `created_at`.
6. `oauth2_authorization_code`: the first write carries the client secret; calls are refused as `unauthorized` until `connection.authorize` (the "connect" button) completes its callback on a platform route and the host writes the token row.
7. `connection.set_credential` is its own ACL operation; the audit log records who and when, never the value.

## Dropped or merged from main

| Main | Becomes |
| --- | --- |
| `registry.integrations` | `integration_type` + `integration_type_version` |
| `registry.integration_digest` | `integration_type_version.digest` |
| `registry.external_system`, `platform.external_system` | kept, identity only, project-scoped |
| `registry.connection` (org-level) | `integration` (identity and type) + `connection` (env instance, all configuration) |
| `registry.connection_generation`, `catalog.connection_generations` | `connection_generation` (parameters and credential reference; no version) |
| `catalog.connection_instances` | `connection` |
| `catalog.connection_requirements` | `connection_requirement`, plus `package_id` and `interfaces` |
| `catalog.connection_bindings`, `registry.environment_connection_binding` (incl. `connector`) | `connection_binding` (no mode) + `release_connection` (the version) |
| `registry.project_connection`, `platform.project_connection` | dropped: the integration belongs to the project |
| `registry.integration_allow`, `platform.integration_allow` | dropped: the binding is the permission |
| `registry.org_connections` | dropped (legacy) |
| `connection.transport`, `connection.exchange` | parameters of the `wamn` type |
| `identity.credential.secret`; project-handle credentials and the `credential_project_handle` index | dropped: secrets live in the store; handles have no writer on main |
| `registry.import_connection_credentials`, `registry.credential_import` | dropped: credentials enter through the UI or API |
| WIT package `wamn:connector` | renamed `wamn:integration` |

## Invariants

Each is checked by the named test type: SQL (a statement against a fresh schema), unit, apply (an `env apply` test) or live (a running host).

| # | Invariant | Check |
| --- | --- | --- |
| 1 | A binding whose connection type differs from the requirement's type cannot exist. | SQL: insert a mismatched `connection_binding` → FK violation |
| 2 | One connection per integration per env. | SQL: second `connection` for (integration, env) → unique violation |
| 3 | A connection's active generation exists and its credential belongs to the same connection. | SQL: point `active_generation` or `credential_id` elsewhere → FK violation at commit |
| 4 | Generations and credential rows never change; they are removed only with their connection. | SQL: UPDATE or DELETE of a generation or credential → 55000; `connection.delete` removes them |
| 5 | Configuration never moves back: `active_generation` only increases. | SQL: set it lower → refused |
| 6 | No plaintext secret in Postgres. | SQL: no column of `identity.credential` holds the value; unit: the write route stores the value only through the store trait |
| 7 | A parameter change in one env never reaches another. | unit: `connection.write` and `connection.copy` write only the target env's generation; copy never sets `credential_id` from the source |
| 8 | Apply never creates, edits or disables a connection, and nothing but apply writes `release_connection` or `connection_binding`. | SQL: the apply role has no write grant on `registry.connection`, `registry.integration`, `registry.connection_generation`; the control route roles have none on the two catalog tables |
| 9 | Every administration action is a control route; `wamn-ctl` connection and integration verbs call routes, never SQL. | unit: each verb in the Headless table maps to a route in `host_route.rs` |
| 10 | At most one active connection per (target, resource\_key). | SQL: second active row → unique violation |
| 11 | An env with an exclusive binding runs one host replica. | apply: a host group body without `replicas: 1` → refused |
| 12 | A pod's readiness never waits on a lease. | live: hold the lease from another session; the new pod reports ready |
| 13 | A `read` requirement never lists a write interface; a read binding on a host type allows only reads. | apply: refused; unit: `authorize_method` and `with_read_only` |
| 14 | The host refuses a call on a name the calling component did not declare. | unit |
| 15 | A version publish keeps the field lifecycle: removal only after deprecation, required only after optional, JSON type never changes, no top-level `auth`, `http_only` auth only with HTTP-interface types, a digest on every wasm and native version. | unit: one refusal per rule |
| 16 | Stored values validate against every version a live release uses of the connection. | apply and unit: a write or a version change that breaks one → refused |
| 17 | After a generation, version or binding change, every host serves the new value within 60 s. | live |
| 18 | One provider call per due OAuth token across pods; a new generation deletes or expires the token row in its own transaction. | live: two hosts refresh one connection → one call; SQL: write a generation → row gone or expired |
| 19 | Rotated secret material is destroyed 5 minutes after the new generation is active. | live |
| 20 | Delete refuses anything referenced (binding, `release_connection`, `platform.file`, connection of an integration, integration or record of an external system). | unit, one refusal per reference |
| 21 | `platform.file.connection_id` references only host-form store types. | SQL or unit: a file row on another type → refused |
| 22 | Names never change. | SQL: UPDATE of `name` on `external_system`, `integration` → refused |
| 23 | A platform upgrade never drops a host-type version a live release uses. | unit: refused, naming the releases |

## Assumptions

| Assumption | How to verify | If false |
| --- | --- | --- |
| A child row's `BEFORE DELETE` trigger, fired by `ON DELETE CASCADE`, sees its parent `registry.connection` row already gone. | SQL test: delete a connection with generations and credentials; the cascade succeeds while a direct DELETE of a generation raises 55000 | `connection.delete` deletes children first under a transaction-local setting the trigger reads |
| A Postgres session advisory lock stays held for the life of its dedicated session and is released when the session drops (pod killed, network cut). | live: take the lock from a host, kill the pod, take it from a second session | the lease becomes a row with a heartbeat and expiry |
| The control host can create and delete Kubernetes Secrets in each env's namespace. | check its RBAC role in the release chart | the Kubernetes backend moves behind a separate writer, or GCP Secret Manager comes first |
| GKE application-layer secrets encryption can be enabled on wamn-dev. | `gcloud container clusters describe` shows `databaseEncryption.state: ENCRYPTED` | Kubernetes Secrets are plaintext in etcd: GCP Secret Manager becomes the first cloud backend |
| A component's imports can be read from its binary at install, so `interfaces` needs no declaration. | unit: decode a built component with `wit-component` and list its imports | `wamn.k` declares the interfaces per name |
| An OAuth refresh token is bound to the client id, not to the client secret, for the providers in use. | test against each provider (NetSuite first): rotate the secret, refresh with the old token | a secret-only rotation deletes the token row and forces `connection.authorize` |
| Integration code holds no state between calls (a fresh `Store` per call). | unit: two calls on one connection share no guest memory | a live version change must wait for runs to finish |
| The host can build a second version's `InstancePre` while serving the first. | live: change the version under load; no failed call | a version change needs a rollout |
| Generated application models emit the FK to `platform.external_system` for provenance columns. | generator unit test on a model with provenance | external system delete must scan the env databases |
