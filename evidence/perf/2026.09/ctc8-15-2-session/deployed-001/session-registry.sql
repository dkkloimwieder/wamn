CREATE ROLE wamn_app NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOINHERIT NOREPLICATION NOBYPASSRLS;
INSERT INTO registry.orgs (id,placement_kind,pool_cluster) VALUES ('acme','pooled','fixture'),('other','pooled','fixture');
INSERT INTO registry.env_policies (org,name,recovery_domain,promotion_rank,instances,storage,cpu,memory,image) VALUES ('acme','dev','"own"',0,1,'1Gi','1','1Gi','postgres:18'),('acme','prod','"own"',1,1,'1Gi','1','1Gi','postgres:18'),('other','dev','"own"',0,1,'1Gi','1','1Gi','postgres:18');
INSERT INTO registry.projects (org,id) VALUES ('acme','shared'),('other','shared');
INSERT INTO registry.project_envs (org,project,env,secret_name,instance_suffix) VALUES ('acme','shared','dev','fixture-acme-dev','k3m9x2p7'),('acme','shared','prod','fixture-acme-prod','p7x2m9k3'),('other','shared','dev','fixture-other-dev','q2n5r8t4');
