{{- define "wamn-platform.systemDatabase" -}}
{{- $namespace := "platform" -}}
{{- $cluster := "wamn-pg" -}}
{{- $name := "wamn_system" -}}
{{- $migration := printf "wamn_platform_sql_%s" (toJson (list "platform-migration" $name $name) | sha256sum | trunc 40) -}}
{{- $migrationRole := dict "name" $migration "login" true "inherit" true "inRoles" (list $name) "disablePassword" true -}}
{{- /* The name follows wamn_provision::identity_role::identity_issuer_login for the platform issuer. */ -}}
{{- $identityRole := dict "name" "wamn_identity_issuer_172c451f63aa4fdc22159b71333ebbf4e46732ee" "login" true "inherit" true "inRoles" (list "wamn_identity_issuer") "disablePassword" true -}}
{{- dict "namespace" $namespace "cluster" $cluster "name" $name "host" (printf "%s-rw.%s.svc.cluster.local" $cluster $namespace) "port" 5432 "ca" (printf "%s-ca" $cluster) "adminTlsSecret" (printf "%s-postgres-tls" $cluster) "identityTlsSecret" "wamn-identity-db-tls" "identityRole" $identityRole "migrationTlsSecret" (printf "%s-tls" ($migration | replace "_" "-")) "migrationRole" $migrationRole | toJson -}}
{{- end -}}

{{- define "wamn-platform.systemDatabasePoolers" -}}
{{- $system := include "wamn-platform.systemDatabase" . | fromJson -}}
{{- $names := dict -}}
{{- range list "control" "apply" -}}
{{- $_ := set $names . (printf "%s-%s" $system.cluster .) -}}
{{- end -}}
{{- dict "names" $names "authRole" "wamn_pooler_auth" "authSecret" "wamn-pg-pooler-auth-tls" | toJson -}}
{{- end -}}

{{- define "wamn-platform.identityReplicas" -}}
2
{{- end -}}

{{- define "wamn-platform.systemDatabaseMaxConnections" -}}
{{- $pooler := .Values.postgres.pooler -}}
{{- $poolerConnections := add $pooler.control.maxDbConnections $pooler.apply.maxDbConnections -}}
{{- /* The auth_query pool uses one login per Pooler, capped by max_user_connections. */ -}}
{{- $passwordLookupConnections := add $pooler.control.maxUserConnections $pooler.apply.maxUserConnections -}}
{{- /* services/identity-service/src/password.rs:51 opens a pool of 8 per replica. */ -}}
{{- $identityConnections := mul (include "wamn-platform.identityReplicas" .) 8 -}}
{{- /* crates/provision/src/provision_org.rs holds one dedicated connection. */ -}}
{{- $provisionOrgConnections := 1 -}}
{{- /* provision_system.rs:32,87,185 holds the admin connection and one extra at a time. */ -}}
{{- $systemSqlConnections := 2 -}}
{{- /* PostgreSQL 18 defaults: superuser_reserved_connections=3, reserved_connections=0. */ -}}
{{- $reservedConnections := 3 -}}
{{- $margin := 10 -}}
{{- add $poolerConnections $passwordLookupConnections $identityConnections $provisionOrgConnections $systemSqlConnections $reservedConnections $margin -}}
{{- end -}}
