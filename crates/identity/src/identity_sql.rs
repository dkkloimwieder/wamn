//! Registry statements shared by identity and environment operations.

pub const ORG_PROJECTS_SQL: &str = "SELECT p.id, CASE WHEN EXISTS (SELECT 1 FROM registry.project_env e JOIN registry.env v USING (org,env) \
    WHERE e.org = p.org AND e.project = p.id AND e.active AND v.active) \
    THEN 'active' ELSE 'inactive' END FROM registry.project p WHERE p.org = $1 \
    AND ($2::text IS NULL OR p.id > $2) ORDER BY p.id LIMIT $3";

pub const CONTROL_PROJECTS_SQL: &str = "SELECT r.id FROM registry.project r \
    JOIN identity.principal p ON p.id=$1 \
    WHERE r.org=$2 AND p.type='user' AND p.active \
    AND EXISTS (SELECT FROM identity.org_membership m WHERE m.principal_id=p.id AND m.org=$2 AND m.active) \
    AND (EXISTS (SELECT FROM identity.project_role a WHERE a.principal_id=p.id AND a.org=$2 AND a.project=r.id AND a.role=$3) \
      OR EXISTS (SELECT FROM identity.org_role a WHERE a.principal_id=p.id AND a.org=$2)) \
    AND ($4::text IS NULL OR r.id>$4) ORDER BY r.id LIMIT $5";

pub const PROJECT_MEMBERS_SQL: &str = "SELECT p.id,p.email,p.display_name, \
    EXISTS (SELECT FROM identity.org_role r WHERE r.principal_id=p.id AND r.org=$1 AND r.role=$4), \
    identity.is_project_admin(p.id,$1,$2) AND $3='project-admin', \
    ARRAY(SELECT e.env FROM registry.project_env e WHERE e.org=$1 AND e.project=$2 AND e.active \
      AND ($8::text IS NULL OR e.env=$8) AND identity.has_project_access(p.id,$1,e.env,$2) ORDER BY e.env) \
    FROM identity.principal p WHERE p.type='user' \
    AND EXISTS (SELECT FROM registry.project_env e WHERE e.org=$1 AND e.project=$2 AND e.active \
      AND ($8::text IS NULL OR e.env=$8) AND identity.has_project_access(p.id,$1,e.env,$2)) \
    AND ($5::text IS NULL OR (p.email,p.id)>($5,$6)) ORDER BY p.email,p.id LIMIT $7";

pub const PROJECT_ENVS_SQL: &str = "SELECT env, e.active FROM registry.project_env p JOIN registry.env e USING (org,env) WHERE p.org = $1 AND p.project = $2 AND p.active \
    AND ($3::text IS NULL OR env > $3) ORDER BY env LIMIT $4";

pub const ENV_IN_PROJECT_SQL: &str = "SELECT EXISTS (SELECT 1 FROM registry.project_env \
    WHERE org = $1 AND project = $2 AND env = $3 AND active)";

pub const PROJECT_IN_ORG_SQL: &str =
    "SELECT EXISTS (SELECT 1 FROM registry.project WHERE org = $1 AND id = $2)";
