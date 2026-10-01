-- The durability selector of each environment policy (wamn-rjtf). Before this
-- file, provision-project-env added the column on every run. A run that mints
-- a PAT issues no schema statement, so the change lives here. The statements
-- are safe on a database that already holds the column.
ALTER TABLE registry.env_policies
    ADD COLUMN IF NOT EXISTS durability_class text DEFAULT 'standard';
ALTER TABLE registry.env_policies
    ALTER COLUMN durability_class SET DEFAULT 'standard';
UPDATE registry.env_policies SET durability_class = 'standard'
 WHERE durability_class IS NULL;
ALTER TABLE registry.env_policies
    ALTER COLUMN durability_class SET NOT NULL;
DO $env_policy_durability$ BEGIN
    IF EXISTS (
        SELECT 1 FROM pg_catalog.pg_constraint AS constraint_row
         WHERE constraint_row.conrelid = 'registry.env_policies'::regclass
           AND constraint_row.conname = 'env_policies_durability_class_check'
           AND pg_catalog.pg_get_constraintdef(constraint_row.oid, true)
               <> 'CHECK (durability_class = ANY (ARRAY[''standard''::text, ''durable''::text]))')
    THEN
        ALTER TABLE registry.env_policies
            DROP CONSTRAINT env_policies_durability_class_check;
    END IF;
    IF NOT EXISTS (
        SELECT 1 FROM pg_catalog.pg_constraint AS constraint_row
         WHERE constraint_row.conrelid = 'registry.env_policies'::regclass
           AND constraint_row.conname = 'env_policies_durability_class_check')
    THEN
        ALTER TABLE registry.env_policies
            ADD CONSTRAINT env_policies_durability_class_check
            CHECK (durability_class IN ('standard', 'durable'));
    END IF;
END $env_policy_durability$;
