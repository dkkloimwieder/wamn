-- platform.assign_counter (docs/plan/platform-config.md, Numbering; owner
-- ruling of wamn-83eth.2) in an installed project database: the writer of
-- the per= key rows of number_counter, beside bump_counter.
--
-- The rest is the NUMBER ASSIGNMENT section of deploy/sql/app-schema.sql,
-- byte for byte. A test in crates/control/provision/src/schema_migrations.rs
-- compares the bytes. A fresh install records this file as applied, because
-- app-schema.sql already holds it.

-- ---------------------------------------------------------------------------
-- NUMBER ASSIGNMENT (docs/plan/platform-config.md, Numbering; owner ruling of
-- wamn-83eth.2). assign_counter is the second of the two platform functions
-- that write number_counter, beside bump_counter. It gives a per= counter one
-- value for each distinct key: under a transaction advisory lock on the
-- (tenant, sequence, counter, key), it returns the value already assigned to
-- the key, or allocates the next value from the counter's '' allocator row
-- and inserts the key row with it. Concurrent first callers of one key wait
-- on the lock, so the allocator advances once for each key, and a rollback
-- gives the value back. The '' key value is the allocator itself, so it is
-- refused with invalid_parameter_value.
--
-- It runs as its owner and checks the caller's tenant as bump_counter does.
-- Project migration 0029 carries this section verbatim, and a test in
-- crates/control/provision/src/schema_migrations.rs compares the bytes.
-- ---------------------------------------------------------------------------
CREATE FUNCTION platform.assign_counter(target_sequence uuid, target_counter text, target_key text)
RETURNS bigint
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog
AS $$
DECLARE
    caller_key text := substring(COALESCE(NULLIF(current_setting('role'), 'none'), session_user::text)
                                 from '^wamn_app_([0-9a-f]{40})_[ab]$');
    sequence_tenant text;
    assigned bigint;
BEGIN
    SELECT s.tenant_id INTO sequence_tenant
      FROM platform.number_sequence AS s
     WHERE s.id = target_sequence
       AND wamn_authority.tenant_key(s.tenant_id) = caller_key;
    IF sequence_tenant IS NULL THEN
        RAISE EXCEPTION 'number sequence % is not a sequence of the caller''s tenant', target_sequence
            USING ERRCODE = 'insufficient_privilege';
    END IF;
    IF target_key = '' THEN
        RAISE EXCEPTION 'the '''' key value is the allocator row of counter %', target_counter
            USING ERRCODE = 'invalid_parameter_value';
    END IF;
    PERFORM pg_advisory_xact_lock(hashtextextended(
        format('number_counter %s %s %s %s', sequence_tenant, target_sequence, target_counter, target_key), 0));
    SELECT c.value INTO assigned
      FROM platform.number_counter AS c
     WHERE c.tenant_id = sequence_tenant
       AND c.sequence_id = target_sequence
       AND c.counter = target_counter
       AND c.key_value = target_key;
    IF assigned IS NOT NULL THEN
        RETURN assigned;
    END IF;
    INSERT INTO platform.number_counter AS c (tenant_id, sequence_id, counter, key_value, value)
    VALUES (sequence_tenant, target_sequence, target_counter, '', 2)
    ON CONFLICT (tenant_id, sequence_id, counter, key_value)
    DO UPDATE SET value = c.value + 1
    RETURNING c.value - 1 INTO assigned;
    INSERT INTO platform.number_counter (tenant_id, sequence_id, counter, key_value, value)
    VALUES (sequence_tenant, target_sequence, target_counter, target_key, assigned);
    RETURN assigned;
END;
$$;
REVOKE ALL ON FUNCTION platform.assign_counter(uuid, text, text) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION platform.assign_counter(uuid, text, text) TO wamn_app;
