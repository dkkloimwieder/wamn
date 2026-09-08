CREATE SCHEMA catalog; CREATE SCHEMA wamn_run;
REVOKE CONNECT, TEMPORARY ON DATABASE "wamn-db-acme--shared--prod--p7x2m9k3" FROM PUBLIC;
INSERT INTO app_system.users (tenant_id,id,email) VALUES ('t2','e00011ba-b82a-40e7-b5ca-aef6f3ab96d9','session@example.invalid');
INSERT INTO app_system.roles (tenant_id,name) VALUES ('t2','administrator');
INSERT INTO app_system.user_roles (tenant_id,user_id,role_name) VALUES ('t2','e00011ba-b82a-40e7-b5ca-aef6f3ab96d9','administrator');
