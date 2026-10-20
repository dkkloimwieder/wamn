-- apply-package creates the stamp and log functions of each package relation
-- in wamn_history (wamn-c8a5v.2). record-history.sql grants CREATE on the
-- schema to wamn_db_owner for a new database. This file grants it to a
-- database that holds an earlier record-history.sql.
GRANT CREATE ON SCHEMA wamn_history TO wamn_db_owner;
