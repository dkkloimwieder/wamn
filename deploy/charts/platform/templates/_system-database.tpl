{{- define "wamn-platform.systemDatabase" -}}
{{- $namespace := "platform" -}}
{{- $cluster := "wamn-pg" -}}
{{- $name := "wamn_system" -}}
{{- $migration := printf "wamn_platform_sql_%s" (toJson (list "platform-migration" $name $name) | sha256sum | trunc 40) -}}
{{- $migrationRole := dict "name" $migration "login" true "inherit" true "inRoles" (list $name) "passwordSecret" (dict "name" ($migration | replace "_" "-")) -}}
{{- /* The name follows wamn_provision::identity_role::identity_issuer_login for the platform issuer. */ -}}
{{- $identityRole := dict "name" "wamn_identity_issuer_172c451f63aa4fdc22159b71333ebbf4e46732ee" "login" true "inherit" true "inRoles" (list "wamn_identity_issuer") "passwordSecret" (dict "name" "wamn-identity-db") -}}
{{- dict "namespace" $namespace "cluster" $cluster "name" $name "host" (printf "%s-rw.%s.svc.cluster.local" $cluster $namespace) "port" 5432 "ca" (printf "%s-ca" $cluster) "adminSecret" (printf "%s-superuser" $cluster) "identityRole" $identityRole "migrationRole" $migrationRole | toJson -}}
{{- end -}}
