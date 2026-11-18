{{- define "project.networkPolicyNames" -}}
{{- $name := .Values.operations.name -}}
{{- dict
    "databaseClients" (printf "%s-allow-project-database-clients" $name)
    "operator" (printf "%s-allow-cnpg-operator" $name)
    "replication" (printf "%s-allow-project-database-replication" $name)
    "cdcPostgres" (printf "%s-allow-cdc-to-postgres" $name)
    "cdcNats" (printf "%s-allow-cdc-to-nats" $name)
    "objectStorage" (printf "%s-allow-database-to-object-storage" $name)
    | toJson -}}
{{- end -}}
