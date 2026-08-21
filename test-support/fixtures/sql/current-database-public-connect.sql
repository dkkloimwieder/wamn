-- Test bootstrap posture for the database this session is connected to.
DO $current_database_public_connect$
BEGIN
    EXECUTE pg_catalog.format(
        'REVOKE CONNECT ON DATABASE %I FROM PUBLIC',
        pg_catalog.current_database()
    );
    ASSERT NOT EXISTS (
        SELECT
        FROM pg_catalog.pg_database AS database
        CROSS JOIN LATERAL pg_catalog.aclexplode(
            COALESCE(
                database.datacl,
                pg_catalog.acldefault('d', database.datdba)
            )
        ) AS acl
        WHERE database.datname = pg_catalog.current_database()
          AND acl.grantee = 0
          AND acl.privilege_type = 'CONNECT'
    ), 'the current database still grants CONNECT to PUBLIC';
END
$current_database_public_connect$;
