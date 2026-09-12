CREATE SCHEMA catalog; CREATE SCHEMA wamn_run;
REVOKE CONNECT, TEMPORARY ON DATABASE "wamn-db-acme--shared--dev--k3m9x2p7" FROM PUBLIC;
INSERT INTO app_system.users (tenant_id,id,email) VALUES ('t1','e00011ba-b82a-40e7-b5ca-aef6f3ab96d9','session@example.invalid');
INSERT INTO app_system.roles (tenant_id,name) VALUES ('t1','member');
INSERT INTO app_system.user_roles (tenant_id,user_id,role_name) VALUES ('t1','e00011ba-b82a-40e7-b5ca-aef6f3ab96d9','member');
