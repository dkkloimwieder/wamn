-- platform.set_counter (docs/plan/platform-config.md, Numbering and
-- Administration; owner ruling of wamn-83eth.3) in an installed project
-- database: the write of the set_counter host route, beside bump_counter and
-- assign_counter.
--
-- The rest is the NUMBER SETTING section of deploy/sql/app-schema.sql, byte
-- for byte. A test in crates/control/provision/src/schema_migrations.rs
-- compares the bytes. A fresh install records this file as applied, because
-- app-schema.sql already holds it.

-- ---------------------------------------------------------------------------
-- NUMBER SETTING (docs/plan/platform-config.md, Numbering and Administration;
-- owner ruling of wamn-83eth.3). set_counter is the third of the three
-- platform functions that write number_counter, beside bump_counter and
-- assign_counter. It is the write of the set_counter host route: under the
-- counter row's lock, it inserts the row of (counter, key) with the value, or
-- sets the value of the row that exists. The value is the next value to
-- render. A value below 1 is refused with invalid_parameter_value. It knows
-- no pattern: the route refuses a per= counter and a set below a live number
-- with the renderer of the wamn:postgres plugin before it commits.
--
-- It runs as its owner and checks the caller's tenant as bump_counter does.
-- Project migration 0030 carries this section verbatim, and a test in
-- crates/control/provision/src/schema_migrations.rs compares the bytes.
-- ---------------------------------------------------------------------------
CREATE FUNCTION platform.set_counter(target_sequence uuid, target_counter text, target_key text,
                                     target_value bigint)
RETURNS void
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog
AS $$
DECLARE
    caller_key text := substring(COALESCE(NULLIF(current_setting('role'), 'none'), session_user::text)
                                 from '^wamn_app_([0-9a-f]{40})_[ab]$');
    sequence_tenant text;
BEGIN
    SELECT s.tenant_id INTO sequence_tenant
      FROM platform.number_sequence AS s
     WHERE s.id = target_sequence
       AND wamn_authority.tenant_key(s.tenant_id) = caller_key;
    IF sequence_tenant IS NULL THEN
        RAISE EXCEPTION 'number sequence % is not a sequence of the caller''s tenant', target_sequence
            USING ERRCODE = 'insufficient_privilege';
    END IF;
    IF target_value IS NULL OR target_value < 1 THEN
        RAISE EXCEPTION 'counter % takes a value of 1 or more, not %', target_counter, target_value
            USING ERRCODE = 'invalid_parameter_value';
    END IF;
    INSERT INTO platform.number_counter AS c (tenant_id, sequence_id, counter, key_value, value)
    VALUES (sequence_tenant, target_sequence, target_counter, target_key, target_value)
    ON CONFLICT (tenant_id, sequence_id, counter, key_value)
    DO UPDATE SET value = EXCLUDED.value;
END;
$$;
REVOKE ALL ON FUNCTION platform.set_counter(uuid, text, text, bigint) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION platform.set_counter(uuid, text, text, bigint) TO wamn_app;
