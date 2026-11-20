-- Targeted reader authority in the control database.
-- Apply after system-schema.sql as wamn_system.
CREATE SCHEMA IF NOT EXISTS catalog AUTHORIZATION wamn_system;
REVOKE ALL ON SCHEMA catalog FROM PUBLIC;

CREATE SCHEMA IF NOT EXISTS wamn_authority AUTHORIZATION wamn_system;
REVOKE ALL ON SCHEMA wamn_authority FROM PUBLIC;

REVOKE ALL ON ALL TABLES IN SCHEMA catalog FROM PUBLIC;
REVOKE ALL ON ALL FUNCTIONS IN SCHEMA catalog FROM PUBLIC;
