{{- define "wamn-platform.systemDatabase" -}}
{{- $namespace := "platform" -}}
{{- $cluster := "wamn-pg" -}}
{{- $name := "wamn_system" -}}
{{- $migration := printf "wamn_platform_sql_%s" (toJson (list "platform-migration" $name $name) | sha256sum | trunc 40) -}}
{{- $migrationRole := dict "name" $migration "login" true "inherit" true "inRoles" (list $name) "disablePassword" true -}}
{{- /* The name follows wamn_platform_install::identity_role::identity_issuer_login for the platform issuer. */ -}}
{{- $identityRole := dict "name" "wamn_identity_issuer_172c451f63aa4fdc22159b71333ebbf4e46732ee" "login" true "inherit" true "inRoles" (list "wamn_identity_issuer") "disablePassword" true -}}
{{- dict "namespace" $namespace "cluster" $cluster "name" $name "host" (printf "%s-rw.%s.svc.cluster.local" $cluster $namespace) "port" 5432 "signingCa" (printf "%s-signing-ca" $cluster) "serverTls" (printf "%s-server-tls" $cluster) "adminTlsSecret" (printf "%s-postgres-tls" $cluster) "identityTlsSecret" "wamn-identity-db-tls" "identityRole" $identityRole "migrationTlsSecret" (printf "%s-tls" ($migration | replace "_" "-")) "migrationRole" $migrationRole | toJson -}}
{{- end -}}

{{- define "wamn-platform.systemDatabasePoolers" -}}
{{- $system := include "wamn-platform.systemDatabase" . | fromJson -}}
{{- $names := dict "control" (printf "%s-control" $system.cluster)
                   "apply" (printf "%s-apply" $system.cluster)
                   "applyTransaction" (printf "%s-apply-transaction" $system.cluster) -}}
{{- dict "names" $names "authRole" "wamn_pooler_auth" "authSecret" "wamn-pg-pooler-auth-tls" | toJson -}}
{{- end -}}

{{- define "wamn-platform.identityReplicas" -}}
2
{{- end -}}
