-- The T1 control-plane REGISTRY storage schema (wamn-q3n.3). The tables that
-- PERSIST the identity/placement model defined by crates/registry — orgs,
-- environments, projects, and the provisioned (org, env, project) databases.
--
-- This is the schema that fills the EMPTY wamn_system DB the T1 cluster
-- (deploy/gcp/cnpg-cluster.yaml) bootstraps: registry-model → registry
-- tables. Project databases independently install the package/effective-release
-- catalog from deploy/sql/catalog-schema.sql.
--
-- PLATFORM-GLOBAL, NOT PROJECT-OWNED — the sharpest difference from
-- catalog-schema.sql: the system DB is the platform's own single-project
-- control-plane state. There are no per-project RLS base grants,
-- NO NULLIF/CHECK(project_id <> '') pattern here. The top-level key is `org_id`;
-- access is the `wamn_system` owner role (from wamn-q3n.2). The control-plane
-- families get their grants from `stable_surface_sql` in wamn-postgres.
--
-- APPLY AS THE `wamn_system` OWNER (it owns the system DB, so it can CREATE
-- SCHEMA): the registry is then owned by — and usable by — that role, which is
-- what .6 provision-org connects as. A superuser driving the apply `SET ROLE
-- wamn_system` first.
--
-- RECORD HISTORY: stamped registry and identity relations carry created_at,
-- created_by, updated_at, and updated_by as NOT NULL columns, and a
-- wamn_record_history_stamp trigger that calls wamn_history.stamp_row(). An applier
-- installs deploy/sql/record-history.sql first. The SYSTEM_SCHEMA_SQL
-- composition in wamn-platform-install carries it. The system database owner,
-- wamn_system, owns its record history helpers. Every write to these relations
-- binds a transaction principal. The identity issuer binds wamn:identity-service
-- for service writes. Provisioning binds wamn:provisioning. Logged relations
-- keep unlimited row images with the actor and operation.
--
-- THE FOUR INVARIANTS for T1, and how this schema
-- encodes / makes each testable:
--   (1) control-plane authority: project workloads cannot connect here.
--       Authorized identity readers perform fresh PAT authentication.
--       The separate identity issuer has explicit key, credential, and throttle grants.
--   (2) no project-database credentials (R8b) — `registry.project_env` stores a Secret
--       *reference* (secret_name + optional secret_namespace) and NO project DB
--       credential column (no url/password/dsn). First-party user login hashes
--       live separately under `identity`. Session signing keys are the explicit
--       authority-only exception: only wamn-identity receives their private part.
--   (3) no project data — the only tables here are the control-plane set below
--       (registry + provisioning + first-party platform identity). No catalogs,
--       run state, payloads, or per-project application users. The live-apply
--       condition asserts the exact table set.
--   (4) dev ≠ prod recovery domain — under D18 this is enforced by the DERIVATION
--       plus crates/registry validate(), not a per-org DB CHECK: two
--       own-domain envs (dev, prod) derive distinct clusters (<org>-dev vs
--       <org>-prod) by construction, so they never collapse; `canary` collapsing
--       onto prod is an INTENTIONAL `recovery-domain: {shared-with: prod}` policy,
--       and `canary` isolated (T4) is `recovery-domain: own` (→ <org>-canary).
--
-- Identifier validators refuse invalid coordinates before resource creation.
-- Stored coordinates also enforce their character and length constraints.
-- ---------------------------------------------------------------------------
-- Schemas. `registry` = the org/project placement model (wamn-q3n.1);
-- `provisioning` = operations state; `identity` = first-party platform principals,
-- local user credential hashes, project-role assignments (wamn-ctc8.6), and
-- Distinct schemas keep each control-plane subsystem namespaced.
-- Owned by the `wamn_system` role the T1 cluster bootstraps (wamn-q3n.2).
-- ---------------------------------------------------------------------------
INSERT INTO platform.login_principal(login, principal_id)
VALUES ('wamn_system', '770df186-ac15-579e-b46b-c297cae2011b')
ON CONFLICT (login) DO NOTHING;

CREATE SCHEMA registry AUTHORIZATION wamn_system;
CREATE SCHEMA provisioning AUTHORIZATION wamn_system;
CREATE SCHEMA identity AUTHORIZATION wamn_system;

-- Platform integration catalog and deployment targets.
CREATE TABLE registry.integration_type (
    code      text PRIMARY KEY CHECK (code ~ '^[a-z][a-z0-9_-]*$'),
    runtime_type text NOT NULL CHECK (runtime_type IN ('host', 'wasm', 'native')),
    exclusive boolean NOT NULL,
    resource_key_template text CHECK (resource_key_template <> ''),
    CHECK (exclusive = (resource_key_template IS NOT NULL))
);
CREATE TABLE registry.integration_type_version (
    integration_type text NOT NULL REFERENCES registry.integration_type (code),
    version          integer NOT NULL CHECK (version > 0),
    digest           text CHECK (digest ~ '^sha256:[0-9a-f]{64}$'),
    parameter_schema jsonb NOT NULL,
    operations       text NOT NULL,
    exports          text[] NOT NULL,
    write_interfaces text[] NOT NULL DEFAULT '{}' CHECK (write_interfaces <@ exports),
    published_at     timestamptz NOT NULL,
    PRIMARY KEY (integration_type, version)
);
CREATE TABLE registry.auth_type (
    code              text PRIMARY KEY,
    credential_schema jsonb NOT NULL,
    parameter_schema  jsonb NOT NULL,
    http_only         boolean NOT NULL
);
CREATE TABLE registry.integration_type_auth (
    integration_type text NOT NULL REFERENCES registry.integration_type (code),
    auth_type        text NOT NULL REFERENCES registry.auth_type (code),
    PRIMARY KEY (integration_type, auth_type)
);

CREATE TABLE registry.target (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    name text NOT NULL UNIQUE,
    type text NOT NULL CHECK (type IN ('local', 'cloud')),
    active boolean NOT NULL DEFAULT true,
    last_seen_at timestamptz
);

-- ---------------------------------------------------------------------------
-- Platform configuration shared by all orgs.
--
-- `platform_domain` is the deployment's platform principal email domain: every
-- project's `platform."user"` platform row carries `<component>@<domain>`
-- (wamn-0h0g.9.15). It belongs to the deployment and not to an org, so it sits
-- on this singleton. It starts NULL because static SQL cannot read deployment
-- configuration; the bootstrap that applies this file sets it, and
-- `reconcile-run-plane` refuses to create a project's platform rows while it is
-- unset. The CHECK carries the same shape and the same two length limits that
-- `wamn_registry::platform_domain::PlatformDomain` enforces: dot-separated
-- labels of lowercase letters, digits and inner hyphens, each label at most 63
-- bytes and the whole name at most 253.
-- ---------------------------------------------------------------------------
CREATE TABLE registry.meta (
    id              boolean PRIMARY KEY DEFAULT true CHECK (id),
    platform_domain text,
    created_at timestamptz NOT NULL,
    created_by uuid NOT NULL,
    updated_at timestamptz NOT NULL,
    updated_by uuid NOT NULL,
    CONSTRAINT meta_platform_domain_check
        CHECK (platform_domain IS NULL
               OR (octet_length(platform_domain) <= 253
                   AND platform_domain ~ '^[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?(\.[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?)*$'))
);
CREATE TRIGGER wamn_record_history_stamp
    BEFORE INSERT OR UPDATE ON registry.meta
    FOR EACH ROW
    EXECUTE FUNCTION wamn_history.stamp_row('created_at', 'created_by', 'updated_at', 'updated_by');

-- ---------------------------------------------------------------------------
-- Organizations are the units of isolation and billing.
-- Double hyphens separate components of derived project names.
-- ---------------------------------------------------------------------------
CREATE TABLE registry.org (
    id             text PRIMARY KEY,
    active         boolean NOT NULL DEFAULT true,
    created_at timestamptz NOT NULL,
    created_by uuid NOT NULL,
    updated_at timestamptz NOT NULL,
    updated_by uuid NOT NULL,
    CONSTRAINT org_id_charset_check
        CHECK (id ~ '^[a-z0-9]+(-[a-z0-9]+)*$'
               AND char_length(id) <= 40
               AND id <> 'wamn' AND id NOT LIKE 'wamn-%')
);
CREATE TRIGGER wamn_record_history_stamp
    BEFORE INSERT OR UPDATE ON registry.org
    FOR EACH ROW
    EXECUTE FUNCTION wamn_history.stamp_row('created_at', 'created_by', 'updated_at', 'updated_by');

-- Org isolation follows the authenticated login.
CREATE FUNCTION registry.login_org() RETURNS text
LANGUAGE sql STABLE SECURITY DEFINER SET search_path = pg_catalog AS $$
    SELECT o.id FROM registry.org o
    CROSS JOIN (VALUES
        ('wamn_control_', 'wamn.control.identity.v0.1'),
        ('wamn_apply_', 'wamn.apply.identity.v0.1')
    ) AS family(prefix, domain)
    WHERE o.active AND starts_with(session_user::text, prefix)
      AND session_user::text = prefix || left(encode(sha256(
          int8send(octet_length(convert_to(domain, 'UTF8'))::bigint) || convert_to(domain, 'UTF8')
          || int8send(3) || convert_to('org', 'UTF8')
          || int8send(octet_length(convert_to(o.id, 'UTF8'))::bigint) || convert_to(o.id, 'UTF8')
          || int8send(8) || convert_to('database', 'UTF8')
          || int8send(octet_length(convert_to(current_database(), 'UTF8'))::bigint)
          || convert_to(current_database(), 'UTF8')
      ), 'hex'), 40)
$$;
REVOKE ALL ON FUNCTION registry.login_org() FROM PUBLIC;


-- ---------------------------------------------------------------------------
-- Projects — structure within an org. Unique per org (the composite PK);
-- FK to the owning org.
-- ---------------------------------------------------------------------------
CREATE TABLE registry.project (
    org  text NOT NULL REFERENCES registry.org (id) ON DELETE RESTRICT,
    id   text NOT NULL,
    active boolean NOT NULL DEFAULT true,
    created_at timestamptz NOT NULL,
    created_by uuid NOT NULL,
    updated_at timestamptz NOT NULL,
    updated_by uuid NOT NULL,
    PRIMARY KEY (org, id),
    -- cjv.20 charset backstop: project `id` is a report_id_issues mirror (lowercase slug
    -- ≤ 40 bytes, not under the reserved `wamn` prefix — it too mints names).
    CONSTRAINT project_id_charset_check
        CHECK (id ~ '^[a-z0-9]+(-[a-z0-9]+)*$'
               AND char_length(id) <= 40
               AND id <> 'wamn' AND id NOT LIKE 'wamn-%')
);
CREATE TRIGGER wamn_record_history_stamp
    BEFORE INSERT OR UPDATE ON registry.project
    FOR EACH ROW
    EXECUTE FUNCTION wamn_history.stamp_row('created_at', 'created_by', 'updated_at', 'updated_by');

-- ---------------------------------------------------------------------------
-- First-party platform identity (wamn-ctc8.6). This is platform-plane
-- authentication state, distinct from per-project `platform` identities.
-- Service principals and their PATs live in project databases.
-- Role slugs are opaque: permission meaning belongs to the management
-- authorization boundary.
--
-- `type` is user or platform. A platform row names a platform
-- component that writes in this database, and only wamn:provisioning does. The
-- row carries its `wamn:<component>` name in `subject` and in `display_name`,
-- and the id that `wamn-identity-protocol` derives from that name.
-- `principal_platform_principal_check` pins the subject, display name, and id
-- of each platform row, and it refuses a `wamn:` display name on another type.
-- The subject pattern refuses a colon on another type. PostgreSQL has no
-- UUIDv5, so a Rust test compares the literal with the derivation. A platform
-- principal cannot authenticate.
--
-- `email` is the deliverable address of a user. `reconcile-run-plane` copies
-- it into the `platform."user"` user row of every project the user can write
-- in (wamn-0h0g.9.18). Only a user carries one: `principal_email_check`
-- refuses an email on another type and refuses a user without one. `subject`
-- is email-shaped by its own CHECK, but it is an authentication token, so
-- nothing reads it as an address.
--
-- `UNIQUE (email)` says the rule plainly: an address names one user. It needs
-- no `type`, because `principal_email_check` already forces NULL on every
-- other type, and PostgreSQL treats repeated NULLs as distinct. So the
-- constraint touches no platform row.
--
-- The refusal belongs here, where the address is entered.
-- `platform."user"` is `UNIQUE (project_id, email)`, so without this the two
-- users collide inside a project instead. That failure stops a whole
-- `reconcile-run-plane` run, unrelated schema repair included.
-- ---------------------------------------------------------------------------
CREATE TABLE identity.principal (
    id           uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    type         text NOT NULL,
    subject      text NOT NULL,
    email        text,
    display_name text NOT NULL,
    active       boolean NOT NULL DEFAULT true,
    created_at   timestamptz NOT NULL,
    created_by   uuid NOT NULL,
    updated_at   timestamptz NOT NULL,
    updated_by   uuid NOT NULL,
    UNIQUE (id, type),
    UNIQUE (type, subject),
    UNIQUE (email),
    CONSTRAINT principal_type_check
        CHECK (type IN ('user', 'platform')),
    CONSTRAINT principal_subject_check
        CHECK (type = 'platform'
               OR (subject ~ '^[a-z0-9][a-z0-9._@+-]*$'
                   AND char_length(subject) <= 254)),
    CONSTRAINT principal_email_check
        CHECK ((type = 'user') = (email IS NOT NULL)
               AND (email IS NULL
                    OR (email ~ '^[^@[:space:]]+@[^@[:space:]]+\.[^@[:space:]]+$'
                        AND char_length(email) <= 254))),
    CONSTRAINT principal_display_name_check
        CHECK (btrim(display_name) <> '' AND char_length(display_name) <= 200),
    CONSTRAINT principal_platform_principal_check
        CHECK (CASE WHEN type = 'platform'
                    THEN (subject, display_name, id) IN (
                             ('wamn:provisioning', 'wamn:provisioning', '770df186-ac15-579e-b46b-c297cae2011b'::uuid),
                             ('wamn:identity-service', 'wamn:identity-service', '63082942-c2ba-5970-9b32-e86ed5c3eb79'::uuid))
                    ELSE display_name NOT LIKE 'wamn:%'
               END)
);
CREATE TRIGGER wamn_record_history_stamp
    BEFORE INSERT OR UPDATE ON identity.principal
    FOR EACH ROW
    EXECUTE FUNCTION wamn_history.stamp_row('created_at', 'created_by', 'updated_at', 'updated_by');

CREATE TABLE identity.project_role (
    principal_id uuid NOT NULL
        REFERENCES identity.principal (id) ON DELETE RESTRICT,
    org          text NOT NULL,
    project      text NOT NULL,
    role         text NOT NULL,
    created_at   timestamptz NOT NULL,
    created_by   uuid NOT NULL,
    updated_at   timestamptz NOT NULL,
    updated_by   uuid NOT NULL,
    PRIMARY KEY (principal_id, org, project, role),
    FOREIGN KEY (org, project)
        REFERENCES registry.project (org, id) ON DELETE RESTRICT,
    CONSTRAINT project_role_role_check
        CHECK (role ~ '^[a-z0-9][a-z0-9-]*$' AND char_length(role) <= 64)
);
CREATE TRIGGER wamn_record_history_stamp
    BEFORE INSERT OR UPDATE ON identity.project_role
    FOR EACH ROW
    EXECUTE FUNCTION wamn_history.stamp_row('created_at', 'created_by', 'updated_at', 'updated_by');

-- ---------------------------------------------------------------------------

-- A user's membership of one org (docs/plan/platform-ui.md §2.7). The
-- principal stays global, and the membership's active flag is org-local: an
-- inactive member keeps the row and holds no access in the org.
CREATE TABLE identity.org_membership (
    principal_id   uuid NOT NULL,
    principal_type text NOT NULL DEFAULT 'user',
    org            text NOT NULL
        REFERENCES registry.org (id) ON DELETE RESTRICT,
    active         boolean NOT NULL DEFAULT true,
    created_at     timestamptz NOT NULL,
    created_by     uuid NOT NULL,
    updated_at     timestamptz NOT NULL,
    updated_by     uuid NOT NULL,
    PRIMARY KEY (principal_id, org),
    FOREIGN KEY (principal_id, principal_type)
        REFERENCES identity.principal (id, type) ON DELETE RESTRICT,
    CONSTRAINT org_membership_user_check
        CHECK (principal_type = 'user')
);
CREATE TRIGGER wamn_record_history_stamp
    BEFORE INSERT OR UPDATE ON identity.org_membership
    FOR EACH ROW
    EXECUTE FUNCTION wamn_history.stamp_row('created_at', 'created_by', 'updated_at', 'updated_by');

-- Serialize new memberships and activations with each other and with the issuer's
-- disable check. Updating a row version also forces stale snapshots to retry.
CREATE FUNCTION identity.check_org_membership() RETURNS trigger
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog AS $$
BEGIN
    IF TG_OP = 'UPDATE' AND NOT NEW.active
       AND (NEW.principal_id, NEW.org) = (OLD.principal_id, OLD.org) THEN
        RETURN NEW;
    END IF;

    UPDATE identity.principal SET display_name = display_name WHERE id = NEW.principal_id;
    IF (COALESCE(pg_has_role(session_user, to_regrole('wamn_control'), 'member'), false)
        OR COALESCE(pg_has_role(session_user, to_regrole('wamn_apply'), 'member'), false))
       AND NOT EXISTS (SELECT FROM pg_roles WHERE rolname = session_user AND rolsuper)
       AND EXISTS (SELECT FROM identity.org_membership
                   WHERE principal_id = NEW.principal_id AND org <> NEW.org) THEN
        RAISE EXCEPTION 'the user already belongs to another org'
            USING ERRCODE = '23514', CONSTRAINT = 'org_membership_single_org';
    END IF;
    RETURN NEW;
END;
$$;
REVOKE ALL ON FUNCTION identity.check_org_membership() FROM PUBLIC;
CREATE TRIGGER check_org_membership
    AFTER INSERT OR UPDATE OF principal_id, org, active ON identity.org_membership
    FOR EACH ROW EXECUTE FUNCTION identity.check_org_membership();

-- A user's administrative role in one org (docs/plan/platform-ui.md §4.4).
-- A role needs the user's membership of the org. Membership deactivation
-- revokes the role explicitly.
CREATE TABLE identity.org_role (
    principal_id uuid NOT NULL,
    org          text NOT NULL,
    role         text NOT NULL,
    created_at   timestamptz NOT NULL,
    created_by   uuid NOT NULL,
    updated_at   timestamptz NOT NULL,
    updated_by   uuid NOT NULL,
    PRIMARY KEY (principal_id, org, role),
    FOREIGN KEY (principal_id, org)
        REFERENCES identity.org_membership (principal_id, org) ON DELETE RESTRICT,
    CONSTRAINT org_role_role_check
        CHECK (role = 'org-admin')
);
CREATE TRIGGER wamn_record_history_stamp
    BEFORE INSERT OR UPDATE ON identity.org_role
    FOR EACH ROW
    EXECUTE FUNCTION wamn_history.stamp_row('created_at', 'created_by', 'updated_at', 'updated_by');

-- Password enrollment (wamn-a045.1). Only user principals can enroll.
-- Password credentials keep stamps and row images. Password tokens keep stamps.
-- The identity library serializes enrollment on the principal row and consumes
-- all outstanding invitations in the same transaction as password creation.
-- Authorized issuer grants belong to the password endpoint implementation.
CREATE TABLE identity.password_credential (
    principal_id uuid PRIMARY KEY,
    principal_type text NOT NULL DEFAULT 'user' CHECK (principal_type = 'user'),
    password_hash text NOT NULL CHECK (octet_length(password_hash) <= 256
                                      AND password_hash LIKE '$argon2id$v=19$%'),
    created_at timestamptz NOT NULL,
    created_by uuid NOT NULL,
    updated_at timestamptz NOT NULL,
    updated_by uuid NOT NULL,
    FOREIGN KEY (principal_id, principal_type)
        REFERENCES identity.principal (id, type) ON DELETE RESTRICT
);
CREATE TRIGGER wamn_record_history_stamp
    BEFORE INSERT OR UPDATE ON identity.password_credential
    FOR EACH ROW
    EXECUTE FUNCTION wamn_history.stamp_row('created_at', 'created_by', 'updated_at', 'updated_by');

CREATE TABLE identity.password_token (
    token_hash text PRIMARY KEY CHECK (token_hash ~ '^sha256:[0-9a-f]{64}$'),
    principal_id uuid NOT NULL,
    principal_type text NOT NULL DEFAULT 'user' CHECK (principal_type = 'user'),
    purpose text NOT NULL CHECK (purpose IN ('invitation', 'reset')),
    expires_at timestamptz NOT NULL,
    consumed_at timestamptz,
    created_at timestamptz NOT NULL,
    created_by uuid NOT NULL,
    updated_at timestamptz NOT NULL,
    updated_by uuid NOT NULL,
    FOREIGN KEY (principal_id, principal_type)
        REFERENCES identity.principal (id, type) ON DELETE RESTRICT,
    CHECK (expires_at > created_at)
);
CREATE INDEX password_token_principal_idx ON identity.password_token (principal_id);
CREATE TRIGGER wamn_record_history_stamp
    BEFORE INSERT OR UPDATE ON identity.password_token
    FOR EACH ROW
    EXECUTE FUNCTION wamn_history.stamp_row('created_at', 'created_by', 'updated_at', 'updated_by');

-- One password login owns a family of single-use renewal credentials.
-- Credential material gets stamps but never row-image history.
CREATE TABLE identity.password_login (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    principal_id uuid NOT NULL,
    principal_type text NOT NULL DEFAULT 'user' CHECK (principal_type = 'user'),
    issuer text NOT NULL CHECK (octet_length(issuer) BETWEEN 1 AND 2048),
    audience text NOT NULL CHECK (octet_length(audience) BETWEEN 1 AND 1024),
    authenticated_at timestamptz NOT NULL,
    expires_at timestamptz NOT NULL,
    renewal_expires_at timestamptz NOT NULL,
    revoked_at timestamptz,
    created_at timestamptz NOT NULL,
    created_by uuid NOT NULL,
    updated_at timestamptz NOT NULL,
    updated_by uuid NOT NULL,
    FOREIGN KEY (principal_id, principal_type)
        REFERENCES identity.principal (id, type) ON DELETE RESTRICT,
    CHECK (expires_at = authenticated_at + interval '8 hours'),
    CHECK (renewal_expires_at > authenticated_at AND renewal_expires_at <= expires_at)
);
CREATE INDEX password_login_principal_idx ON identity.password_login (principal_id);
CREATE INDEX password_login_expiry_idx ON identity.password_login (issuer, expires_at);
CREATE TRIGGER wamn_record_history_stamp
    BEFORE INSERT OR UPDATE ON identity.password_login
    FOR EACH ROW
    EXECUTE FUNCTION wamn_history.stamp_row('created_at', 'created_by', 'updated_at', 'updated_by');

CREATE TABLE identity.renewal_credential (
    token_hash text PRIMARY KEY CHECK (token_hash ~ '^sha256:[0-9a-f]{64}$'),
    login_id uuid NOT NULL REFERENCES identity.password_login (id) ON DELETE CASCADE,
    consumed_at timestamptz,
    created_at timestamptz NOT NULL,
    created_by uuid NOT NULL,
    updated_at timestamptz NOT NULL,
    updated_by uuid NOT NULL
);
CREATE INDEX renewal_credential_login_idx ON identity.renewal_credential (login_id);
CREATE TRIGGER wamn_record_history_stamp
    BEFORE INSERT OR UPDATE ON identity.renewal_credential
    FOR EACH ROW
    EXECUTE FUNCTION wamn_history.stamp_row('created_at', 'created_by', 'updated_at', 'updated_by');

-- The principal lock orders disablement against every login writer. The trigger
-- also covers administrative status changes outside the identity Rust library.
CREATE FUNCTION identity.revoke_disabled_password_login() RETURNS trigger
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog AS $$
BEGIN
    UPDATE identity.password_login SET revoked_at = clock_timestamp()
    WHERE principal_id = NEW.id AND revoked_at IS NULL;
    RETURN NEW;
END;
$$;
REVOKE ALL ON FUNCTION identity.revoke_disabled_password_login() FROM PUBLIC;
CREATE TRIGGER revoke_disabled_password_login
    AFTER UPDATE OF active ON identity.principal
    FOR EACH ROW WHEN (NOT NEW.active)
    EXECUTE FUNCTION identity.revoke_disabled_password_login();

-- never per-session state. A fresh UUID kid is generated for each publication;
-- rotation never reuses the credential machinery's A/B slot names as kids.
-- Only the identity authority receives table access. Hosts receive a public
-- projection through JWKS, never a database grant or private key material.
--
-- Signers hold a SHARE lock on their issuer's signing-state row through the
-- signature. Activation and compromise removal hold UPDATE locks on that row.
-- After old signers drain, activation records clock_timestamp() as a confirmed
-- signing cutoff and erases the old private key in the same transaction.
-- The public key remains valid through cutoff + 930 seconds (900 + 30).
-- A failed flip rolls back both state and cutoff; no last-use/token writes are
-- needed. Compromise removal deletes the key immediately and never falls back.
CREATE TABLE identity.session_key (
    issuer         text NOT NULL CHECK (btrim(issuer) <> ''),
    kid            uuid NOT NULL DEFAULT gen_random_uuid(),
    public_key     bytea NOT NULL CHECK (octet_length(public_key) = 32),
    private_key_envelope  bytea,
    published_at   timestamptz NOT NULL DEFAULT clock_timestamp(),
    signing_cutoff timestamptz,
    created_at timestamptz NOT NULL,
    created_by uuid NOT NULL,
    updated_at timestamptz NOT NULL,
    updated_by uuid NOT NULL,
    PRIMARY KEY (issuer, kid),
    CONSTRAINT session_key_retirement_check
        CHECK ((signing_cutoff IS NULL) = (private_key_envelope IS NOT NULL))
);
CREATE TRIGGER wamn_record_history_stamp
    BEFORE INSERT OR UPDATE ON identity.session_key
    FOR EACH ROW
    EXECUTE FUNCTION wamn_history.stamp_row('created_at', 'created_by', 'updated_at', 'updated_by');

-- Identity owns the NATS operator and account signing keys. Only the
-- identity issuer opens the envelopes and publishes account revisions.
CREATE TABLE identity.nats_key (
    issuer text NOT NULL CHECK (btrim(issuer) <> ''),
    kind text NOT NULL CHECK (kind IN (
        'operator-root', 'operator-signing', 'system-account',
        'system-signing', 'platform-account', 'platform-signing',
        'runtime-operator', 'env', 'env-signing'
    )),
    org text REFERENCES registry.org (id) ON DELETE RESTRICT,
    env text,
    public_key text NOT NULL,
    private_key_envelope bytea NOT NULL,
    project_count integer CHECK (project_count >= 0),
    -- Keep the staged claim and its byte reservation across publication failures.
    claim_limits jsonb,
    reserved_disk_bytes bigint CHECK (reserved_disk_bytes >= 0),
    published boolean,
    issued_at bigint NOT NULL DEFAULT floor(EXTRACT(EPOCH FROM clock_timestamp()))::bigint,
    created_at timestamptz NOT NULL,
    created_by uuid NOT NULL,
    updated_at timestamptz NOT NULL,
    updated_by uuid NOT NULL,
    UNIQUE NULLS NOT DISTINCT (issuer, kind, org, env),
    CHECK (CASE WHEN kind IN ('env', 'env-signing')
        THEN org IS NOT NULL AND env IS NOT NULL
        ELSE org IS NULL AND env IS NULL END),
    CHECK ((kind = 'env') = (project_count IS NOT NULL)
       AND (kind = 'env') = (claim_limits IS NOT NULL)
       AND (kind = 'env') = (reserved_disk_bytes IS NOT NULL)
       AND (kind = 'env') = (published IS NOT NULL))
);
CREATE TRIGGER wamn_record_history_stamp
    BEFORE INSERT OR UPDATE ON identity.nats_key
    FOR EACH ROW
    EXECUTE FUNCTION wamn_history.stamp_row('created_at', 'created_by', 'updated_at', 'updated_by');

DO $identity_issuer_grants$ BEGIN
    IF to_regrole('wamn_identity_issuer') IS NOT NULL THEN
        GRANT EXECUTE ON FUNCTION platform.prune_tx_identity() TO wamn_identity_issuer;
        GRANT SELECT, INSERT ON identity.nats_key TO wamn_identity_issuer;
        GRANT UPDATE (project_count, issued_at, claim_limits, reserved_disk_bytes, published) ON identity.nats_key TO wamn_identity_issuer;
    END IF;
END $identity_issuer_grants$;

CREATE TABLE identity.session_signing_state (
    issuer     text PRIMARY KEY CHECK (btrim(issuer) <> ''),
    active_kid uuid,
    created_at timestamptz NOT NULL,
    created_by uuid NOT NULL,
    updated_at timestamptz NOT NULL,
    updated_by uuid NOT NULL,
    FOREIGN KEY (issuer, active_kid)
        REFERENCES identity.session_key (issuer, kid)
);
CREATE TRIGGER wamn_record_history_stamp
    BEFORE INSERT OR UPDATE ON identity.session_signing_state
    FOR EACH ROW
    EXECUTE FUNCTION wamn_history.stamp_row('created_at', 'created_by', 'updated_at', 'updated_by');

-- ---------------------------------------------------------------------------
-- Project-envs — the registry LEAF: one provisioned (org, project, env)
-- database, keyed by the identity triple, with a REFERENCE to its credential
-- Secret. `env` is a validated slug.
--
-- INVARIANT 2 (no credentials, R8b): `secret_name` (+ optional
-- `secret_namespace`) is a REFERENCE — the actual credential material lives in a
-- K8s Secret resolved by a component holding the matching RBAC. There is NO
-- url/password/dsn column here; compromise of this DB yields the org/placement
-- *list*, never the keys to any org's data.
--
-- `instance_suffix` identifies one provisioning of the triple. Physical
-- names include the suffix so a new provisioning uses new resources.
-- Deactivation retains the row. Re-adding the project replaces the suffix
-- and the prior suffix remains in row history.
--
-- `disposable` marks an environment whose admitted facts a development loop may
-- REPLACE (wamn-10yt.38). `wamn dev up` provisions its own target with it true;
-- every other provisioning leaves the `false` default, which is what keeps every
-- existing environment's admitted component fact immutable BY CONSTRUCTION
-- rather than by a caller remembering to withhold a flag. The condition is
-- anchored in THIS row, never in a CLI argument or a stage: the target decides.
-- Lifecycle state belongs to registry.env. Project database state belongs to its deployment.
--
-- The active flag controls admission (docs/plan/platform-ui.md §5.4). Identity
-- offers no audience of an inactive environment, and its host serves none of
-- its routes. CDC, data and grants stay as they are.
-- ---------------------------------------------------------------------------
-- An environment owns the readiness budget, drain bound, project fact cache lifetime, and lifecycle of its project deployments.
CREATE TABLE registry.env (
    org text NOT NULL REFERENCES registry.org(id) ON DELETE RESTRICT,
    env text NOT NULL CHECK (env ~ '^[a-z0-9]+(-[a-z0-9]+)*$' AND char_length(env) <= 40),
    readiness_budget_seconds int NOT NULL DEFAULT 600 CHECK (readiness_budget_seconds > 0),
    drain_bound_seconds int NOT NULL DEFAULT 300 CHECK (drain_bound_seconds > 0),
    revocation_seconds int NOT NULL DEFAULT 60 CHECK (revocation_seconds BETWEEN 0 AND 3600),
    active boolean NOT NULL DEFAULT true,
    disposable boolean NOT NULL DEFAULT false,
    -- The platform database owner sets an environment's NATS limits.
    nats_connections bigint NOT NULL DEFAULT 64 CHECK (nats_connections > 0),
    nats_consumers bigint NOT NULL DEFAULT 2000 CHECK (nats_consumers > 0),
    nats_subscriptions bigint NOT NULL DEFAULT 10000 CHECK (nats_subscriptions > 0),
    nats_payload_bytes bigint NOT NULL DEFAULT 1048576 CHECK (nats_payload_bytes BETWEEN 1 AND 1048576),
    nats_streams_per_project bigint NOT NULL DEFAULT 2 CHECK (nats_streams_per_project > 0),
    nats_disk_bytes_per_project bigint NOT NULL DEFAULT 536870912 CHECK (nats_disk_bytes_per_project > 0),
    created_at timestamptz NOT NULL,
    created_by uuid NOT NULL,
    updated_at timestamptz NOT NULL,
    updated_by uuid NOT NULL,
    PRIMARY KEY (org, env)
);
CREATE TRIGGER wamn_record_history_stamp BEFORE INSERT OR UPDATE ON registry.env
    FOR EACH ROW EXECUTE FUNCTION wamn_history.stamp_row('created_at', 'created_by', 'updated_at', 'updated_by');

CREATE TABLE registry.platform_event_type (
    code text PRIMARY KEY,
    label text NOT NULL
);
INSERT INTO registry.platform_event_type (code, label) VALUES
    ('org_create', 'Create org'),
    ('env_apply', 'Apply env'),
    ('project_env_deactivate', 'Deactivate project in env'),
    ('deploy', 'Deploy app'),
    ('rollback', 'Roll back app'),
    ('schema_apply', 'Apply schema'),
    ('schema_reset', 'Reset schema'),
    ('capture_gap_repair', 'Repair capture gap');

CREATE TABLE registry.platform_event_outcome (
    code text PRIMARY KEY,
    label text NOT NULL,
    success boolean CHECK ((success IS NULL) = (code = 'started'))
);
INSERT INTO registry.platform_event_outcome (code, label, success) VALUES
    ('started', 'Started', NULL),
    ('succeeded', 'Succeeded', true),
    ('failed', 'Failed', false),
    ('rolled_back', 'Rolled back', false),
    ('refused', 'Refused', false);

CREATE TABLE provisioning.platform_event (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    occurred_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    command_id uuid NOT NULL,
    event_type text NOT NULL REFERENCES registry.platform_event_type (code),
    outcome text NOT NULL REFERENCES registry.platform_event_outcome (code),
    principal_id uuid NOT NULL REFERENCES identity.principal (id),
    org text NOT NULL CHECK (org ~ '^[a-z0-9]+(-[a-z0-9]+)*$'
                             AND char_length(org) <= 40
                             AND org <> 'wamn' AND org NOT LIKE 'wamn-%'),
    env text CHECK (env ~ '^[a-z0-9]+(-[a-z0-9]+)*$' AND char_length(env) <= 40),
    project text CHECK (project ~ '^[a-z0-9]+(-[a-z0-9]+)*$'
                        AND char_length(project) <= 40
                        AND project <> 'wamn' AND project NOT LIKE 'wamn-%'),
    app text,
    package_digest text CHECK (package_digest ~ '^sha256:[0-9a-f]{64}$'),
    helm_revision integer,
    job_namespace text,
    job_name text,
    failure_summary text CHECK (
        failure_summary IS NULL OR (outcome = 'failed' AND char_length(failure_summary) <= 500)
    )
);
CREATE INDEX platform_event_command_id ON provisioning.platform_event (command_id);
CREATE INDEX platform_event_org_env_occurred_at ON provisioning.platform_event (org, env, occurred_at);
CREATE INDEX platform_event_org_project_occurred_at ON provisioning.platform_event (org, project, occurred_at);

CREATE FUNCTION provisioning.reject_immutable_row_change() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION USING
        ERRCODE = '55000',
        MESSAGE = TG_TABLE_SCHEMA || '.' || TG_TABLE_NAME || ' is immutable';
END
$$;
REVOKE ALL ON FUNCTION provisioning.reject_immutable_row_change() FROM PUBLIC;
CREATE TRIGGER platform_event_immutable
    BEFORE UPDATE OR DELETE ON provisioning.platform_event
    FOR EACH ROW EXECUTE FUNCTION provisioning.reject_immutable_row_change();

ALTER TABLE provisioning.platform_event ENABLE ROW LEVEL SECURITY;

-- Identity appends its terminal authentication decisions. Platform operators read them.
CREATE TABLE identity.authentication_event_type (
    code text PRIMARY KEY,
    label text NOT NULL
);
INSERT INTO identity.authentication_event_type (code, label) VALUES
    ('password_sign_in', 'Sign in with password'),
    ('session_renew', 'Renew session'),
    ('session_sign_out', 'Sign out of session'),
    ('session_sign_out_all', 'Sign out of all sessions'),
    ('password_recovery_request', 'Request password recovery'),
    ('password_reset', 'Reset password'),
    ('invitation_request', 'Request enrollment invitation'),
    ('password_enroll', 'Accept invitation and enroll password'),
    ('nats_workload_credential_issue', 'Issue workload NATS credential'),
    ('nats_host_credential_issue', 'Issue host NATS credential');

CREATE TABLE identity.authentication_event_outcome (
    code text PRIMARY KEY,
    label text NOT NULL,
    success boolean NOT NULL
);
INSERT INTO identity.authentication_event_outcome (code, label, success) VALUES
    ('succeeded', 'Succeeded', true),
    ('delivery_accepted', 'Accepted for delivery', true),
    ('request_invalid', 'Invalid request', false),
    ('email_invalid', 'Email input refused', false),
    ('password_policy_refused', 'Password policy refused', false),
    ('unknown_email', 'No user at attempted email', false),
    ('bad_password', 'Password did not match', false),
    ('principal_unknown', 'Principal was not found', false),
    ('principal_inactive', 'Principal is inactive', false),
    ('credential_missing', 'Password credential missing', false),
    ('credential_corrupt', 'Stored password credential unusable', false),
    ('audience_invalid', 'Invalid org audience', false),
    ('org_unknown', 'Org was not found', false),
    ('org_inactive', 'Org is inactive', false),
    ('membership_missing', 'Org membership missing', false),
    ('membership_inactive', 'Org membership is inactive', false),
    ('global_rate_limited', 'Global rate limit reached', false),
    ('source_rate_limited', 'Source rate limit reached', false),
    ('account_rate_limited', 'Account rate limit reached', false),
    ('request_busy', 'Request capacity exhausted', false),
    ('password_work_busy', 'Password work capacity exhausted', false),
    ('token_malformed', 'Credential format or type refused', false),
    ('token_unknown', 'No matching credential', false),
    ('token_consumed', 'Email credential already consumed', false),
    ('token_expired', 'Credential expired', false),
    ('renewal_inactive', 'Renewal inactivity window expired', false),
    ('renewal_revoked', 'Login family revoked', false),
    ('renewal_reused', 'Consumed renewal credential replayed', false),
    ('already_enrolled', 'Password already enrolled', false),
    ('address_changed', 'Account email changed during recovery', false),
    ('delivery_failed', 'Credential delivery failed', false),
    ('delivery_cleanup_failed', 'Failed delivery credential could not be consumed', false),
    ('workload_refused', 'Workload credential refused', false),
    ('org_mismatch', 'Requested org differs from workload org', false),
    ('public_key_invalid', 'NATS public key refused', false),
    ('nats_account_missing', 'NATS environment account missing', false),
    ('signing_failed', 'Credential signing failed', false),
    ('unavailable', 'Authentication dependency unavailable', false),
    ('timed_out', 'Authentication request deadline elapsed', false);

CREATE TABLE identity.authentication_event (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    event_type text NOT NULL REFERENCES identity.authentication_event_type (code),
    outcome text NOT NULL REFERENCES identity.authentication_event_outcome (code),
    occurred_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    principal_id uuid REFERENCES identity.principal (id) ON DELETE RESTRICT,
    attempted_email text CHECK (char_length(attempted_email) <= 512),
    ip_address inet NOT NULL CHECK (masklen(ip_address) = CASE family(ip_address) WHEN 4 THEN 32 ELSE 128 END),
    ip_source text NOT NULL CHECK (ip_source IN ('socket', 'forwarded')),
    peer_ip_address inet CHECK (masklen(peer_ip_address) = CASE family(peer_ip_address) WHEN 4 THEN 32 ELSE 128 END),
    CHECK ((ip_source = 'forwarded') = (peer_ip_address IS NOT NULL)),
    org text CHECK (org ~ '^[a-z0-9]+(-[a-z0-9]+)*$' AND char_length(org) <= 40
                    AND org <> 'wamn' AND org NOT LIKE 'wamn-%'),
    failure_summary text CHECK (
        failure_summary IS NULL OR (outcome NOT IN ('succeeded', 'delivery_accepted')
                                    AND char_length(failure_summary) BETWEEN 1 AND 500)
    ),
    login_id uuid,
    session_jti text CHECK (session_jti ~ '^[0-9a-f]{64}$'),
    workload text CHECK (char_length(workload) BETWEEN 1 AND 253)
);
CREATE TRIGGER authentication_event_immutable
    BEFORE UPDATE OR DELETE ON identity.authentication_event
    FOR EACH ROW EXECUTE FUNCTION provisioning.reject_immutable_row_change();

CREATE INDEX authentication_event_sign_in_email_time
    ON identity.authentication_event (attempted_email, occurred_at DESC, id DESC)
    WHERE event_type = 'password_sign_in' AND attempted_email IS NOT NULL;
CREATE INDEX authentication_event_principal_time
    ON identity.authentication_event (principal_id, occurred_at DESC, id DESC)
    WHERE principal_id IS NOT NULL;
CREATE INDEX authentication_event_org_time
    ON identity.authentication_event (org, occurred_at DESC, id DESC)
    WHERE org IS NOT NULL;

ALTER TABLE identity.authentication_event ENABLE ROW LEVEL SECURITY;
DO $authentication_event$
BEGIN
    IF to_regrole('wamn_identity_issuer') IS NOT NULL THEN
        CREATE POLICY issuer_append ON identity.authentication_event
            FOR INSERT TO wamn_identity_issuer WITH CHECK (true);
        GRANT INSERT (event_type, outcome, principal_id, attempted_email,
                      ip_address, ip_source, peer_ip_address, org, failure_summary,
                      login_id, session_jti, workload)
            ON identity.authentication_event TO wamn_identity_issuer;
    END IF;
END
$authentication_event$;

CREATE TABLE registry.project_env (
    org              text NOT NULL,
    env              text NOT NULL,
    project          text NOT NULL,
    secret_name      text NOT NULL,
    secret_namespace text,
    instance_suffix  text NOT NULL,
    active boolean NOT NULL DEFAULT true,
    schema_mode text CHECK (schema_mode IN ('reset', 'migrate')),
    created_at timestamptz NOT NULL,
    created_by uuid NOT NULL,
    updated_at timestamptz NOT NULL,
    updated_by uuid NOT NULL,
    PRIMARY KEY (org, env, project),
    FOREIGN KEY (org, project) REFERENCES registry.project (org, id) ON DELETE RESTRICT,
    FOREIGN KEY (org, env) REFERENCES registry.env(org, env) ON DELETE RESTRICT,
    CONSTRAINT project_env_instance_suffix_check
        CHECK (instance_suffix ~ '^[a-z0-9]{8}$')
);
CREATE TRIGGER wamn_record_history_stamp
    BEFORE INSERT OR UPDATE ON registry.project_env
    FOR EACH ROW
    EXECUTE FUNCTION wamn_history.stamp_row('created_at', 'created_by', 'updated_at', 'updated_by');

-- Applied settings belong to the project deployment.
-- Helm revisions record deployment attempts and supply rollback history.
CREATE TABLE registry.env_app (
    org text NOT NULL,
    env text NOT NULL,
    project text NOT NULL,
    app text NOT NULL CHECK (app <> ''),
    active boolean NOT NULL DEFAULT true,
    label text NOT NULL CHECK (
        octet_length(label) BETWEEN 1 AND 21
        AND label ~ '^[a-z0-9]+(-[a-z0-9]+)*$'),
    client_package text CHECK (client_package <> ''),
    client_web text CHECK (client_web ~ '^sha256:[0-9a-f]{64}$'),
    PRIMARY KEY (org, env, project, app),
    UNIQUE (org, label),
    FOREIGN KEY (org, env, project)
        REFERENCES registry.project_env (org, env, project) ON DELETE RESTRICT,
    CHECK ((client_package IS NULL) = (client_web IS NULL))
);

CREATE TABLE registry.env_integration (
    org text NOT NULL,
    env text NOT NULL,
    name text NOT NULL CHECK (name ~ '^[a-z][a-z0-9_-]{0,62}$'),
    version integer NOT NULL CHECK (version > 0),
    PRIMARY KEY (org, env, name),
    FOREIGN KEY (org, env)
        REFERENCES registry.env (org, env) ON DELETE RESTRICT
);

CREATE TABLE registry.env_app_connection (
    org text NOT NULL,
    env text NOT NULL,
    project text NOT NULL,
    app text NOT NULL,
    alias text NOT NULL CHECK (alias <> ''),
    connection_name text NOT NULL,
    PRIMARY KEY (org, env, project, app, alias),
    FOREIGN KEY (org, env, project, app)
        REFERENCES registry.env_app (org, env, project, app) ON DELETE RESTRICT,
    FOREIGN KEY (org, env, connection_name)
        REFERENCES registry.env_integration (org, env, name)
);

-- User project access is an explicit grant to one project-environment
-- (wamn-ctc8.19). Keep the platform-issued principal UUID: platform."user"
-- uses that same UUID for the environment's roles and permissions. Management
-- project roles do not grant membership, and services use their existing
-- environment-bound authentication. Deactivation removes the grants so a
-- later provisioning of the same triple starts without explicit memberships.
CREATE TABLE identity.project_env_membership (
    principal_id   uuid NOT NULL,
    principal_type text NOT NULL DEFAULT 'user',
    org            text NOT NULL,
    project        text NOT NULL,
    env            text NOT NULL,
    created_at     timestamptz NOT NULL,
    created_by     uuid NOT NULL,
    updated_at     timestamptz NOT NULL,
    updated_by     uuid NOT NULL,
    PRIMARY KEY (principal_id, org, env, project),
    FOREIGN KEY (principal_id, principal_type)
        REFERENCES identity.principal (id, type) ON DELETE RESTRICT,
    FOREIGN KEY (org, env, project)
        REFERENCES registry.project_env (org, env, project) ON DELETE RESTRICT,
    CONSTRAINT project_env_membership_user_check
        CHECK (principal_type = 'user')
);
CREATE TRIGGER wamn_record_history_stamp
    BEFORE INSERT OR UPDATE ON identity.project_env_membership
    FOR EACH ROW
    EXECUTE FUNCTION wamn_history.stamp_row('created_at', 'created_by', 'updated_at', 'updated_by');

-- Serialize membership grants with the deployment update that revokes them.
-- AFTER INSERT runs only for rows that pass the caller's RLS policy.
CREATE FUNCTION identity.require_active_project_env() RETURNS trigger
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog AS $$
BEGIN
    PERFORM 1 FROM registry.project_env
    WHERE org = NEW.org AND env = NEW.env AND project = NEW.project AND active
    FOR SHARE;
    IF NOT FOUND THEN
        RAISE EXCEPTION 'project env is inactive or missing' USING ERRCODE = '23503';
    END IF;
    RETURN NULL;
END $$;
REVOKE ALL ON FUNCTION identity.require_active_project_env() FROM PUBLIC;

CREATE TRIGGER project_env_membership_require_active
    AFTER INSERT OR UPDATE OF org, env, project ON identity.project_env_membership
    FOR EACH ROW EXECUTE FUNCTION identity.require_active_project_env();

-- ---------------------------------------------------------------------------
-- Event readers — the CDC capture registrations (D19 v3, wamn-l5i9.9). One row
-- per project-env with CDC enabled: the publication + failover replication slot
-- the reader streams from (the `wamn_cdc_…` Postgres objects the
-- environment provisioning creates), the JetStream stream envelopes
-- land in (one stream per organization, project, and environment), and a
-- REFERENCE to the reader's
-- replication-credential Secret (`wamn-cdc-<org>--<project>--<env>`).
--
-- INVARIANT 2 (no credentials, R8b): `replication_secret_name` (+ optional
-- namespace) is a reference only — the replication credential is its own tier
-- ABOVE the wamn_app query credential, and its material lives in a K8s Secret,
-- never here. Keyed by the identity triple; FK to the project-env it captures
-- through the same project-env identity. Reserving a new generation clears its
-- previous registration. The reader configuration comes from a mounted JSON
-- file. Control uses the active deployment's registration to locate the
-- source stream and slot during recovery.
--
-- `schema` records the application schemas covered by the publication.
-- Environment provisioning owns that declaration. The reader does not read
-- registrations or compare the publication shape. Control stops the reader
-- for capture recovery and restarts it after resynchronization.
-- ---------------------------------------------------------------------------
CREATE TABLE registry.event_reader (
    org                          text NOT NULL,
    project                      text NOT NULL,
    env                          text NOT NULL,
    publication                  text NOT NULL,
    schema                       text[],
    slot                         text NOT NULL,
    stream                       text NOT NULL,
    replication_secret_name      text NOT NULL,
    replication_secret_namespace text,
    enabled                      boolean NOT NULL DEFAULT true,
    created_at                   timestamptz NOT NULL DEFAULT now(),
    updated_at                   timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (org, env, project),
    FOREIGN KEY (org, env, project)
        REFERENCES registry.project_env (org, env, project) ON DELETE RESTRICT
);

-- ---------------------------------------------------------------------------
-- Capture gaps (wamn-59z6, docs/operations/gcp.md#57-cdc-slot-lost): one row
-- per lost slot of a CDC reader registration. `wamn recover-capture-gap`
-- writes it, and `wamn close-capture-gap` sets `resync_at`. The reader
-- exits on a lost slot. Control stops its Deployment before recovery and
-- restarts it after the operator closes the gap. `slot` keeps the lost slot's name. A null `start_lsn`
-- means "from registration": the source stream held no event. `created_at` is
-- the time of the recovery verb.
-- ---------------------------------------------------------------------------
CREATE TABLE registry.capture_gap (
    org        text NOT NULL,
    project    text NOT NULL,
    env        text NOT NULL,
    slot       text NOT NULL,
    start_lsn  pg_lsn,
    start_at   timestamptz NOT NULL,
    reason     text NOT NULL,
    end_lsn    pg_lsn NOT NULL,
    resync_at  timestamptz,
    created_at timestamptz NOT NULL,
    created_by uuid NOT NULL,
    updated_at timestamptz NOT NULL,
    updated_by uuid NOT NULL,
    PRIMARY KEY (org, project, env, created_at),
    FOREIGN KEY (org, env, project)
        REFERENCES registry.event_reader (org, env, project) ON DELETE CASCADE
);
CREATE TRIGGER wamn_record_history_stamp
    BEFORE INSERT OR UPDATE ON registry.capture_gap
    FOR EACH ROW
    EXECUTE FUNCTION wamn_history.stamp_row('created_at', 'created_by', 'updated_at', 'updated_by');

-- ---------------------------------------------------------------------------
-- The issuer locks a principal without principal mutation privileges.
CREATE FUNCTION identity.lock_password_principal(principal uuid) RETURNS boolean
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog AS $$
DECLARE eligible boolean;
BEGIN
    SELECT p.active AND p.type = 'user' INTO eligible
    FROM identity.principal AS p WHERE p.id = $1 FOR UPDATE;
    RETURN COALESCE(eligible, false);
END;
$$;
REVOKE ALL ON FUNCTION identity.lock_password_principal(uuid) FROM PUBLIC;

-- Fixed-window password admission shared by every identity replica.
CREATE TABLE identity.password_attempt (
    bucket text PRIMARY KEY CHECK (octet_length(bucket) <= 160),
    started_at timestamptz NOT NULL,
    attempts integer NOT NULL CHECK (attempts > 0),
    created_at timestamptz NOT NULL,
    created_by uuid NOT NULL,
    updated_at timestamptz NOT NULL,
    updated_by uuid NOT NULL
);

-- An environment keeps its target identity.
ALTER TABLE registry.env ADD COLUMN target_id uuid REFERENCES registry.target(id);
CREATE FUNCTION registry.keep_env_target() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF OLD.target_id IS NOT NULL AND NEW.target_id IS DISTINCT FROM OLD.target_id THEN
        RAISE EXCEPTION 'env_target_changed' USING ERRCODE = '55000';
    END IF;
    RETURN NEW;
END $$;
REVOKE ALL ON FUNCTION registry.keep_env_target() FROM PUBLIC;
CREATE TRIGGER env_target_immutable BEFORE UPDATE OF target_id ON registry.env
    FOR EACH ROW EXECUTE FUNCTION registry.keep_env_target();

-- Administration reads and writes use the same restricted function families.
CREATE FUNCTION registry.configure_integration_type(org_id text, operation text, payload jsonb) RETURNS jsonb
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog AS $$
BEGIN
    IF (COALESCE(pg_has_role(session_user, to_regrole('wamn_control'), 'member'), false)
        OR COALESCE(pg_has_role(session_user, to_regrole('wamn_apply'), 'member'), false))
       AND NOT EXISTS (SELECT FROM pg_roles WHERE rolname = session_user AND rolsuper)
       AND org_id IS DISTINCT FROM registry.login_org() THEN
        RAISE EXCEPTION 'org access refused' USING ERRCODE='42501';
    END IF;
    IF operation <> 'integration_type.list' THEN
        RETURN jsonb_build_object('error','invalid_input','field','operation');
    END IF;
    RETURN jsonb_build_object('integration_types', COALESCE((
        SELECT jsonb_agg(to_jsonb(t) || jsonb_build_object(
            'versions',(SELECT jsonb_agg(to_jsonb(v) ORDER BY v.version)
                        FROM registry.integration_type_version v WHERE v.integration_type=t.code),
            'auth_types',(SELECT jsonb_agg(a.auth_type ORDER BY a.auth_type)
                          FROM registry.integration_type_auth a WHERE a.integration_type=t.code)) ORDER BY t.code)
        FROM registry.integration_type t), '[]'::jsonb));
END $$;

ALTER FUNCTION registry.configure_integration_type(text,text,jsonb) OWNER TO wamn_system;
REVOKE ALL ON FUNCTION registry.configure_integration_type(text,text,jsonb) FROM PUBLIC;
DO $integration_type_acl$ BEGIN
    IF to_regrole('wamn_control') IS NOT NULL THEN
        GRANT USAGE ON SCHEMA registry TO wamn_control;
        GRANT EXECUTE ON FUNCTION registry.configure_integration_type(text,text,jsonb) TO wamn_control;
    END IF;
END $integration_type_acl$;

CREATE TRIGGER wamn_record_history_stamp
    BEFORE INSERT OR UPDATE ON identity.password_attempt
    FOR EACH ROW
    EXECUTE FUNCTION wamn_history.stamp_row('created_at', 'created_by', 'updated_at', 'updated_by');

-- Install history before provisioning seeds so the owner login records them.
DO $record_history$
DECLARE source record;
BEGIN
    FOR source IN
        SELECT schemaname, tablename FROM pg_catalog.pg_tables
        WHERE schemaname = 'registry'
        UNION ALL
        SELECT 'identity', unnest(ARRAY[
            'principal', 'org_membership', 'org_role', 'project_role',
            'project_env_membership', 'password_credential'
        ])
    LOOP
        PERFORM wamn_history.create_history_table(source.schemaname, source.tablename, false);
        EXECUTE format('CREATE TRIGGER wamn_record_history_log AFTER INSERT OR UPDATE OR DELETE '
                       'ON %I.%I FOR EACH ROW EXECUTE FUNCTION wamn_history.log_row_change(''unlimited'')',
                       source.schemaname, source.tablename);
        IF to_regrole('wamn_platform') IS NOT NULL THEN
            EXECUTE format('REVOKE ALL ON %I.%I FROM wamn_platform',
                           source.schemaname, source.tablename || '_history');
        END IF;
    END LOOP;
END
$record_history$;

INSERT INTO registry.integration_type (code, runtime_type, exclusive, resource_key_template) VALUES
    ('http', 'host', false, NULL), ('gcs', 'host', false, NULL), ('s3', 'host', false, NULL),
    ('modbus', 'wasm', true, 'tcp:{authority}');

INSERT INTO registry.integration_type_version
    (integration_type, version, digest, parameter_schema, operations, exports, write_interfaces, published_at) VALUES
    ('http', 1, NULL,
     '{"type":"object","properties":{"primary-authority":{"type":"string","minLength":1,"immutable":true},"tls-verification":{"type":"string","enum":["disabled","verify-authority"]}},"required":["primary-authority","tls-verification"]}', 'wamn:connection/http', ARRAY['wamn:connection/http@0.1.0'], '{}', transaction_timestamp()),
    ('gcs', 1, NULL,
     '{"type":"object","properties":{"provider":{"type":"string","enum":["gcs"],"immutable":true},"container":{"type":"string","minLength":1,"immutable":true},"prefix":{"type":"string","minLength":1,"immutable":true},"cdn_host":{"type":"string","minLength":1}},"required":["container","prefix","provider"]}', 'wasmcloud:blobstore/imports@0.1.0', ARRAY['wasmcloud:blobstore/types@0.1.0','wasmcloud:blobstore/container@0.1.0','wasmcloud:blobstore/blobstore@0.1.0'], '{}', transaction_timestamp()),
    ('s3', 1, NULL,
     '{"type":"object","properties":{"provider":{"type":"string","enum":["s3"],"immutable":true},"container":{"type":"string","minLength":1,"immutable":true},"prefix":{"type":"string","minLength":1,"immutable":true},"cdn_host":{"type":"string","minLength":1},"endpoint":{"type":"string","minLength":1,"immutable":true}},"required":["container","prefix","endpoint"]}', 'wasmcloud:blobstore/imports@0.1.0', ARRAY['wasmcloud:blobstore/types@0.1.0','wasmcloud:blobstore/container@0.1.0','wasmcloud:blobstore/blobstore@0.1.0'], '{}', transaction_timestamp()),
    ('modbus', 1, 'sha256:37c3fc98df0a7b1cdf230dfa16956d34e832b6f1198a04293932170674e0d682',
     '{"type":"object","properties":{},"additionalProperties":false}', 'wamn:modbus/client@1.0.0', ARRAY['wamn:modbus/read@1.0.0','wamn:modbus/write@1.0.0','wamn:integration/health@0.1.0'], ARRAY['wamn:modbus/write@1.0.0'], transaction_timestamp());

INSERT INTO registry.auth_type (code, credential_schema, parameter_schema, http_only) VALUES
    ('http_basic', '{"type":"object","properties":{"username":{"type":"string"},"password":{"type":"string"}},"required":["username","password"]}',
     '{"type":"object","properties":{},"additionalProperties":false}', true),
    ('api_key', '{"type":"object","properties":{"key":{"type":"string"}},"required":["key"]}',
     '{"type":"object","properties":{"in":{"type":"string","enum":["header","query"]},"name":{"type":"string"}},"required":["in","name"],"additionalProperties":false}', true),
    ('hmac', '{"type":"object","properties":{"secret":{"type":"string","minLength":1}},"required":["secret"],"additionalProperties":false}',
     '{"type":"object","properties":{"algorithm":{"type":"string","enum":["sha256"]},"header":{"type":"string","minLength":1}},"required":["algorithm","header"],"additionalProperties":false}', true),
    ('oauth2_client_credentials', '{"type":"object","properties":{"client_secret":{"type":"string"}},"required":["client_secret"]}',
     '{"type":"object","properties":{"client_id":{"type":"string"},"token_url":{"type":"string"},"token_authority":{"type":"string","minLength":1},"scopes":{"type":"array","items":{"type":"string"}}},"required":["client_id","token_url","scopes"],"additionalProperties":false}', true),
    ('oauth2_client_credentials_jwt', '{"type":"object","properties":{"private_key":{"type":"string"}},"required":["private_key"],"additionalProperties":false}',
     '{"type":"object","properties":{"client_id":{"type":"string"},"token_url":{"type":"string"},"token_authority":{"type":"string","minLength":1},"scopes":{"type":"array","items":{"type":"string"}},"key_id":{"type":"string"},"algorithm":{"type":"string","enum":["PS256","ES256","RS256"]},"scope_separator":{"type":"string","enum":[" ",","],"default":" "}},"required":["client_id","token_url","key_id","algorithm"],"additionalProperties":false}', true),
    ('oauth2_authorization_code', '{"type":"object","properties":{"client_secret":{"type":"string"}},"required":["client_secret"]}',
     '{"type":"object","properties":{"client_id":{"type":"string"},"authorize_url":{"type":"string"},"token_url":{"type":"string"},"token_authority":{"type":"string","minLength":1},"scopes":{"type":"array","items":{"type":"string"}}},"required":["client_id","authorize_url","token_url","scopes"],"additionalProperties":false}', true),
    ('aws_sigv4', '{"type":"object","properties":{"access_key_id":{"type":"string"},"secret_access_key":{"type":"string"}},"required":["access_key_id","secret_access_key"]}',
     '{"type":"object","properties":{"region":{"type":"string"},"service":{"type":"string"}},"required":["region","service"],"additionalProperties":false}', true),
    ('custom', '{"type":"object"}',
     '{"type":"object","properties":{},"additionalProperties":false}', false),
    ('none', '{"type":"object","properties":{},"additionalProperties":false}',
     '{"type":"object","properties":{},"additionalProperties":false}', false);

INSERT INTO registry.integration_type_auth (integration_type, auth_type) VALUES
    ('http', 'http_basic'),
    ('http', 'api_key'),
    ('http', 'hmac'),
    ('http', 'oauth2_client_credentials'),
    ('http', 'oauth2_client_credentials_jwt'),
    ('http', 'oauth2_authorization_code'),
    ('http', 'aws_sigv4'),
    ('http', 'none'),
    ('gcs', 'custom'),
    ('s3', 'custom'),
    ('modbus', 'none');

INSERT INTO registry.target (name, type) VALUES ('cloud', 'cloud');

-- The deployment metadata seed uses the schema owner's login principal.
DO $meta$ BEGIN
  INSERT INTO registry.meta DEFAULT VALUES;
END $meta$;

-- The system owner stamps both platform principals as wamn:provisioning.
DO $provisioning$ BEGIN
  INSERT INTO identity.principal (id, type, subject, display_name)
  VALUES ('770df186-ac15-579e-b46b-c297cae2011b', 'platform',
          'wamn:provisioning', 'wamn:provisioning'),
         ('63082942-c2ba-5970-9b32-e86ed5c3eb79', 'platform',
          'wamn:identity-service', 'wamn:identity-service');
END $provisioning$;

DO $control_surface$ BEGIN
  IF EXISTS (SELECT FROM pg_roles WHERE rolname = 'wamn_control') THEN
    EXECUTE format('GRANT CONNECT ON DATABASE %I TO wamn_control', current_database());
    REVOKE ALL PRIVILEGES ON ALL TABLES IN SCHEMA "identity", "provisioning", "registry" FROM "wamn_control"; REVOKE ALL PRIVILEGES ON SCHEMA "identity", "provisioning", "registry" FROM "wamn_control"; GRANT USAGE ON SCHEMA "identity", "registry" TO "wamn_control"; GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE "identity"."org_membership" TO "wamn_control"; GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE "identity"."org_role" TO "wamn_control"; GRANT SELECT ON TABLE "identity"."password_login" TO "wamn_control"; GRANT SELECT, INSERT, UPDATE ON TABLE "identity"."principal" TO "wamn_control"; GRANT SELECT (platform_domain) ON registry.meta TO wamn_control; GRANT SELECT ON registry.integration_type, registry.integration_type_version, registry.integration_type_auth, registry.auth_type TO wamn_control; GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE "identity"."project_env_membership" TO "wamn_control"; GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE "identity"."project_role" TO "wamn_control"; GRANT SELECT ON TABLE "registry"."project_env", "registry"."env_app", "registry"."env_app_connection", "registry"."env_integration", "registry"."target" TO "wamn_control"; GRANT SELECT, INSERT ON TABLE "registry"."project" TO "wamn_control"; GRANT UPDATE ("active") ON TABLE "registry"."env", "registry"."project" TO "wamn_control";
    GRANT SELECT (id, active) ON registry.org TO wamn_control;
    GRANT USAGE ON SCHEMA wamn_history TO wamn_control;
    GRANT EXECUTE ON FUNCTION wamn_history.row_image(record) TO wamn_control;
    GRANT INSERT ON identity.principal_history, identity.org_membership_history,
        identity.org_role_history, identity.project_role_history,
        identity.project_env_membership_history, registry.project_history,
        registry.env_history TO wamn_control;
  END IF;
END $control_surface$;

CREATE FUNCTION registry.login_principal(principal uuid) RETURNS boolean
LANGUAGE sql STABLE SECURITY DEFINER SET search_path=pg_catalog AS $$
    SELECT EXISTS (SELECT FROM identity.org_membership m WHERE m.principal_id=principal AND m.org=registry.login_org())
$$;
REVOKE ALL ON FUNCTION registry.login_principal(uuid) FROM PUBLIC;

CREATE FUNCTION identity.principal_in_login_org_only(principal uuid) RETURNS boolean
LANGUAGE sql STABLE SECURITY DEFINER SET search_path = pg_catalog AS $$
    SELECT COALESCE(bool_and(m.org = (SELECT registry.login_org())), false)
    FROM identity.org_membership m WHERE m.principal_id = principal
$$;
REVOKE ALL ON FUNCTION identity.principal_in_login_org_only(uuid) FROM PUBLIC;

DO $identity_membership_read$ BEGIN
    IF to_regrole('wamn_system_reader') IS NOT NULL THEN
        GRANT SELECT (principal_id, org, active) ON identity.org_membership TO wamn_system_reader;
        GRANT EXECUTE ON FUNCTION registry.login_org(), registry.login_principal(uuid) TO wamn_system_reader;
    END IF;
END $identity_membership_read$;

DO $control_security$
DECLARE
    relation_name text;
    predicate text;
    -- Resolve the group roles at query time so this also works before bootstrap creates them.
    org_login constant text := $predicate$
        (COALESCE(pg_has_role(session_user, to_regrole('wamn_control'), 'member'), false)
         OR COALESCE(pg_has_role(session_user, to_regrole('wamn_apply'), 'member'), false))
        AND NOT EXISTS (SELECT FROM pg_roles WHERE rolname = session_user AND rolsuper)
    $predicate$;
    bound_actor constant text := '(SELECT wamn_authority.current_principal_id()) IS NOT NULL';
BEGIN
    FOREACH relation_name IN ARRAY ARRAY[
        'registry.org', 'registry.project',
        'registry.env', 'registry.project_env',
        'registry.env_app', 'registry.env_app_connection',
        'registry.env_integration', 'registry.event_reader',
        'registry.capture_gap',
        'identity.org_membership', 'identity.org_role',
        'identity.project_role', 'identity.project_env_membership'
    ] LOOP
        predicate := CASE relation_name
          WHEN 'registry.org' THEN 'id = (SELECT registry.login_org())'
          ELSE 'org = (SELECT registry.login_org())' END;
        predicate := format('CASE WHEN %s THEN %s ELSE true END', org_login, predicate);
        EXECUTE format('ALTER TABLE %s ENABLE ROW LEVEL SECURITY', relation_name);
        EXECUTE format('CREATE POLICY login_org ON %s USING (%s) WITH CHECK (%s AND %s)',
                       relation_name, predicate, bound_actor, predicate);
    END LOOP;

    EXECUTE format('CREATE POLICY login_org ON provisioning.platform_event
        USING (CASE WHEN %s THEN org = (SELECT registry.login_org())
            AND principal_id = (SELECT wamn_authority.current_principal_id())
            ELSE false END)', org_login);

    ALTER TABLE identity.principal ENABLE ROW LEVEL SECURITY;
    EXECUTE format('CREATE POLICY login_org_select ON identity.principal FOR SELECT
                    USING (CASE WHEN %s THEN registry.login_principal(id) ELSE true END)', org_login);
    EXECUTE format('CREATE POLICY login_org_insert ON identity.principal FOR INSERT
                    WITH CHECK (%s)', bound_actor);
    predicate := format('CASE WHEN %s THEN identity.principal_in_login_org_only(id) ELSE true END',
                        org_login);
    EXECUTE format('CREATE POLICY login_org_update ON identity.principal FOR UPDATE
                    USING (%s) WITH CHECK (%s AND %s)', predicate, bound_actor, predicate);

    ALTER TABLE identity.password_login ENABLE ROW LEVEL SECURITY;
    predicate := format('CASE WHEN %s THEN registry.login_principal(principal_id) ELSE true END',
                        org_login);
    EXECUTE format('CREATE POLICY login_org ON identity.password_login FOR ALL
                    USING (%s) WITH CHECK (%s)', predicate, predicate);

    IF to_regrole('wamn_control') IS NOT NULL THEN
        GRANT EXECUTE ON FUNCTION registry.login_org(), registry.login_principal(uuid),
            identity.principal_in_login_org_only(uuid) TO wamn_control;
        GRANT SELECT ON registry.env TO wamn_control;
        GRANT USAGE ON SCHEMA provisioning TO wamn_control;
        GRANT INSERT, SELECT (command_id, outcome) ON provisioning.platform_event TO wamn_control;
    END IF;
END
$control_security$;

-- Membership projection keeps grants current while a project is inactive.
CREATE FUNCTION identity.project_admin_grant(principal uuid, org_id text, project_id text)
RETURNS boolean LANGUAGE sql STABLE SECURITY DEFINER SET search_path = pg_catalog AS $$
    SELECT EXISTS (SELECT FROM identity.principal p
        JOIN identity.org_membership m ON m.principal_id=p.id AND m.org=org_id AND m.active
        WHERE p.id=principal AND p.type='user' AND p.active
        AND (NOT ((COALESCE(pg_has_role(session_user, to_regrole('wamn_control'), 'member'), false)
                   OR COALESCE(pg_has_role(session_user, to_regrole('wamn_apply'), 'member'), false))
                  AND NOT EXISTS (SELECT FROM pg_roles WHERE rolname = session_user AND rolsuper))
             OR org_id = registry.login_org())
        AND (EXISTS (SELECT FROM identity.org_role r WHERE r.principal_id=p.id AND r.org=org_id AND r.role='org-admin')
          OR EXISTS (SELECT FROM identity.project_role r WHERE r.principal_id=p.id AND r.org=org_id AND r.project=project_id AND r.role='project-admin')))
$$;
REVOKE ALL ON FUNCTION identity.project_admin_grant(uuid,text,text) FROM PUBLIC;

CREATE FUNCTION identity.project_access_grant(principal uuid, org_id text, env_name text, project_id text)
RETURNS boolean LANGUAGE sql STABLE SECURITY DEFINER SET search_path = pg_catalog AS $$
    SELECT (NOT ((COALESCE(pg_has_role(session_user, to_regrole('wamn_control'), 'member'), false)
                  OR COALESCE(pg_has_role(session_user, to_regrole('wamn_apply'), 'member'), false))
                 AND NOT EXISTS (SELECT FROM pg_roles WHERE rolname = session_user AND rolsuper))
            OR org_id = registry.login_org())
    AND EXISTS (SELECT FROM registry.project_env e WHERE e.org=org_id AND e.env=env_name AND e.project=project_id AND e.active)
    AND (identity.project_admin_grant(principal,org_id,project_id)
      OR EXISTS (SELECT FROM identity.project_env_membership m
        JOIN identity.org_membership om USING (principal_id,org)
        JOIN identity.principal p ON p.id=m.principal_id
        WHERE m.principal_id=principal AND m.org=org_id AND m.env=env_name AND m.project=project_id
          AND om.active AND p.active AND p.type='user'))
$$;
REVOKE ALL ON FUNCTION identity.project_access_grant(uuid,text,text,text) FROM PUBLIC;
-- Operational authorization also checks activity on every request.
-- An org admin may name a missing project so the route can report its own refusal.
CREATE FUNCTION identity.is_project_admin(principal uuid, org_id text, project_id text)
RETURNS boolean LANGUAGE sql STABLE SECURITY DEFINER SET search_path = pg_catalog AS $$
    SELECT EXISTS (SELECT FROM registry.org o WHERE o.id=org_id AND o.active)
    AND NOT EXISTS (SELECT FROM registry.project p
                    WHERE p.org=org_id AND p.id=project_id AND NOT p.active)
    AND identity.project_admin_grant(principal,org_id,project_id)
$$;
REVOKE ALL ON FUNCTION identity.is_project_admin(uuid,text,text) FROM PUBLIC;

CREATE FUNCTION identity.has_project_access(principal uuid, org_id text, env_name text, project_id text)
RETURNS boolean LANGUAGE sql STABLE SECURITY DEFINER SET search_path = pg_catalog AS $$
    SELECT EXISTS (SELECT FROM registry.project p JOIN registry.org o ON o.id=p.org
                   WHERE p.org=org_id AND p.id=project_id AND p.active AND o.active)
    AND identity.project_access_grant(principal,org_id,env_name,project_id)
$$;
REVOKE ALL ON FUNCTION identity.has_project_access(uuid,text,text,text) FROM PUBLIC;
DO $authority_grants$
DECLARE reader text;
BEGIN
    FOREACH reader IN ARRAY ARRAY['wamn_system_reader','wamn_identity_issuer','wamn_control','wamn_system','wamn_apply'] LOOP
        IF to_regrole(reader) IS NOT NULL THEN
            EXECUTE format('GRANT EXECUTE ON FUNCTION identity.is_project_admin(uuid,text,text), identity.has_project_access(uuid,text,text,text) TO %I',reader);
        END IF;
    END LOOP;
    IF to_regrole('wamn_control') IS NOT NULL THEN
        GRANT EXECUTE ON FUNCTION identity.project_admin_grant(uuid,text,text),
            identity.project_access_grant(uuid,text,text,text) TO wamn_control;
    END IF;
END $authority_grants$;
