-- Record history grants for wamn_app, the stable guest role of a tenant or
-- package database.
--
-- This file carries no transaction of its own. CATALOG_SCHEMA_SQL composes it
-- after record-history.sql. Other tenant and package appliers apply it after
-- record-history.sql in their own transaction. The system database never
-- applies it. The file names wamn_app, so an applier without that role fails.
--
-- wamn_app writes logged relations, and the log function calls
-- wamn_history.row_image with the authority of the writer. A history read
-- renders the current row through wamn_history.row_image or
-- wamn_history.timestamptz_image.
GRANT USAGE ON SCHEMA wamn_history TO wamn_app;
GRANT EXECUTE ON FUNCTION wamn_history.row_image(record) TO wamn_app;
GRANT EXECUTE ON FUNCTION wamn_history.timestamptz_image(timestamptz) TO wamn_app;
