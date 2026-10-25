-- Package IDs own application objects. The database owner retains platform objects.
-- Match provision::package_owner_role and its history grants. Existing grants
-- survive ALTER OWNER, including column grants and identity sequences.
DO $package_owners$
DECLARE
    package text;
    owner_role text;
    item record;
    object_name text;
BEGIN
    PERFORM pg_advisory_xact_lock(hashtext('wamn_role_bootstrap'));
    FOR package IN SELECT DISTINCT owner_package_id FROM catalog.package_definition_owners LOOP
        owner_role := 'wamn_package_owner_' || left(encode(sha256(convert_to(package, 'UTF8')), 'hex'), 40);
        IF NOT EXISTS (SELECT FROM pg_catalog.pg_roles WHERE rolname = owner_role) THEN
            BEGIN
                EXECUTE format('CREATE ROLE %I NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOINHERIT NOREPLICATION NOBYPASSRLS', owner_role);
            EXCEPTION WHEN duplicate_object THEN NULL;
            END;
        END IF;
        IF EXISTS (SELECT FROM pg_catalog.pg_roles WHERE rolname = 'wamn_provisioner')
           AND NOT EXISTS (SELECT FROM pg_catalog.pg_auth_members m
               JOIN pg_catalog.pg_roles r ON r.oid = m.roleid
               JOIN pg_catalog.pg_roles p ON p.oid = m.member
               WHERE r.rolname = owner_role AND p.rolname = 'wamn_provisioner'
               GROUP BY m.roleid, m.member
               HAVING bool_or(m.admin_option) AND bool_or(m.inherit_option) AND bool_or(m.set_option)) THEN
            EXECUTE format('GRANT %I TO wamn_provisioner WITH ADMIN TRUE, INHERIT TRUE, SET TRUE', owner_role);
        END IF;
        EXECUTE format('GRANT USAGE, CREATE ON SCHEMA wamn_history TO %I', owner_role);
        EXECUTE format('GRANT EXECUTE ON FUNCTION wamn_history.stamp_row(), wamn_history.create_history_table(text,text,boolean), wamn_history.row_image(record), wamn_history.timestamptz_image(timestamptz), wamn_history.log_row_change() TO %I', owner_role);
        FOR item IN
            SELECT DISTINCT schema_name FROM catalog.package_definition_owners
            WHERE owner_package_id = package
        LOOP
            EXECUTE format('GRANT USAGE, CREATE ON SCHEMA %I TO %I', item.schema_name, owner_role);
        END LOOP;
        FOR item IN
            SELECT DISTINCT schema_name, relation_name FROM catalog.package_definition_owners
            WHERE owner_package_id = package AND definition_type = 'relation'
        LOOP
            EXECUTE format('ALTER TABLE %I.%I OWNER TO %I', item.schema_name, item.relation_name, owner_role);
            IF to_regclass(format('%I.%I', item.schema_name, item.relation_name || '_history')) IS NOT NULL THEN
                EXECUTE format('ALTER TABLE %I.%I OWNER TO %I', item.schema_name, item.relation_name || '_history', owner_role);
            END IF;
            FOREACH object_name IN ARRAY ARRAY[item.schema_name || '__' || item.relation_name || '_stamp', item.schema_name || '__' || item.relation_name || '_log'] LOOP
                IF to_regprocedure(format('wamn_history.%I()', object_name)) IS NOT NULL THEN
                    EXECUTE format('ALTER FUNCTION wamn_history.%I() OWNER TO %I', object_name, owner_role);
                END IF;
            END LOOP;
        END LOOP;
        FOR item IN
            SELECT DISTINCT schema_name, definition_name FROM catalog.package_definition_owners
            WHERE owner_package_id = package AND definition_type = 'synchronization_function'
        LOOP
            EXECUTE format('ALTER FUNCTION %I.%I() OWNER TO %I', item.schema_name, item.definition_name, owner_role);
        END LOOP;
    END LOOP;
    -- A schema shared with an overlay belongs to its earliest relation owner.
    FOR item IN
        SELECT DISTINCT ON (o.schema_name) o.schema_name, o.owner_package_id
        FROM catalog.package_definition_owners o
        JOIN catalog.packages p ON p.package_id = o.owner_package_id AND p.tenant_id = o.tenant_id
        JOIN pg_catalog.pg_namespace n ON n.nspname = o.schema_name
        JOIN pg_catalog.pg_class c ON c.relnamespace = n.oid AND c.relname = o.relation_name
        WHERE o.definition_type = 'relation'
        ORDER BY o.schema_name, p.registered_at, c.oid, o.owner_package_id
    LOOP
        owner_role := 'wamn_package_owner_' || left(encode(sha256(convert_to(item.owner_package_id, 'UTF8')), 'hex'), 40);
        EXECUTE format('ALTER SCHEMA %I OWNER TO %I', item.schema_name, owner_role);
    END LOOP;
END
$package_owners$;
