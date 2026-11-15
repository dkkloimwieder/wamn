DO $$ BEGIN
    BEGIN
        DELETE FROM registry.connection_generation
        WHERE connection_id='00000000-0000-0000-0000-000000000031' AND generation=1;
        ASSERT false, 'direct generation deletion must refuse';
    EXCEPTION WHEN SQLSTATE '55000' THEN NULL; END;
    BEGIN
        UPDATE registry.connection_generation SET parameters='{"changed":true}'
        WHERE connection_id='00000000-0000-0000-0000-000000000031' AND generation=1;
        ASSERT false, 'generation updates must refuse';
    EXCEPTION WHEN SQLSTATE '55000' THEN NULL; END;
    BEGIN
        UPDATE registry.connection_generation SET generation=generation
        WHERE connection_id='00000000-0000-0000-0000-000000000031' AND generation=1;
        ASSERT false, 'generation updates must refuse even when values stay equal';
    EXCEPTION WHEN SQLSTATE '55000' THEN NULL; END;
    BEGIN
        UPDATE identity.credential SET version='2'
        WHERE id='00000000-0000-0000-0000-000000000041';
        ASSERT false, 'credential updates must refuse';
    EXCEPTION WHEN SQLSTATE '55000' THEN NULL; END;
    BEGIN
        UPDATE identity.credential SET version=version
        WHERE id='00000000-0000-0000-0000-000000000041';
        ASSERT false, 'credential updates must refuse even when values stay equal';
    EXCEPTION WHEN SQLSTATE '55000' THEN NULL; END;
    BEGIN
        DELETE FROM identity.credential WHERE id='00000000-0000-0000-0000-000000000041';
        ASSERT false, 'direct credential deletion must refuse';
    EXCEPTION WHEN SQLSTATE '55000' THEN NULL; END;

    DELETE FROM registry.connection WHERE id='00000000-0000-0000-0000-000000000031';
    ASSERT NOT EXISTS (SELECT FROM registry.connection_generation
                       WHERE connection_id='00000000-0000-0000-0000-000000000031');
    ASSERT NOT EXISTS (SELECT FROM identity.credential
                       WHERE connection_id='00000000-0000-0000-0000-000000000031');
    ASSERT EXISTS (SELECT FROM registry.connection_generation
                   WHERE connection_id='00000000-0000-0000-0000-000000000032');
    ASSERT EXISTS (SELECT FROM identity.credential
                   WHERE connection_id='00000000-0000-0000-0000-000000000032');
END $$;
