-- Portable package, effective-release, authoring, and attestation storage for
-- the T1 control database. Apply after system-schema.sql as wamn_system.
--
-- This is a greenfield declaration. It carries no catalog-document reader,
-- schema-model persistence, upgrade shim, or flow-era release relation.

CREATE EXTENSION IF NOT EXISTS pgcrypto;
CREATE SCHEMA IF NOT EXISTS catalog AUTHORIZATION wamn_system;
CREATE SCHEMA IF NOT EXISTS wamn_run AUTHORIZATION wamn_system;
REVOKE ALL ON SCHEMA catalog, wamn_run FROM PUBLIC;

CREATE OR REPLACE FUNCTION catalog.reject_immutable_row_change()