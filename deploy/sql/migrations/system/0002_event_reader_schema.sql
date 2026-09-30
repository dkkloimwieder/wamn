-- The schema that each CDC reader registration covers (wamn-0h0g.19.21).
-- A row from before this file has no schema, and the reader refuses it.
-- enable-cdc-project-env writes the schema on its next run.
ALTER TABLE registry.event_readers ADD COLUMN schema text;
