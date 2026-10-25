-- Preserve each registered schema as the first member of the capture set.
ALTER TABLE registry.event_readers
    ALTER COLUMN schema TYPE text[]
    USING CASE WHEN schema IS NULL THEN NULL ELSE ARRAY[schema] END;
