-- Project-environment package, release, graph, and registration storage.
--
-- Package directories are the authoring truth: raw `wamn.json` bytes identify
-- an immutable package coordinate and ordered migration bytes are recorded in
-- the same transaction that applies them. Effective releases independently
-- compose exact package coordinates. No catalog-document, schema-model, or
-- generated semantic-table registry survives this schema.

CREATE SCHEMA catalog AUTHORIZATION CURRENT_USER;


GRANT USAGE ON SCHEMA catalog TO wamn_app;

CREATE FUNCTION catalog.reject_immutable_row_change()