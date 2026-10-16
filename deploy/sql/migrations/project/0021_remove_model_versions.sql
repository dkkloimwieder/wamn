-- The WAL position supplies weak ETags without a row shared by writers.
-- CASCADE removes the note and deferred bump triggers on application relations.
DROP SCHEMA IF EXISTS wamn_cache CASCADE;
