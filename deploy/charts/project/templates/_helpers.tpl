{{- define "project.networkPolicyNames" -}}
{{- $name := .Values.operations.name -}}
{{- dict
    "databaseClients" (printf "%s-allow-project-database-clients" $name)
    "workerToDatabase" (printf "%s-allow-worker-to-project-database" $name)
    "operator" (printf "%s-allow-cnpg-operator" .Values.operations.cluster)
    "replication" (printf "%s-allow-project-database-replication" $name)
    "cdcPostgres" (printf "%s-allow-cdc-to-postgres" $name)
    "cdcNats" (printf "%s-allow-cdc-to-nats" $name)
    "objectStorage" (printf "%s-allow-database-to-object-storage" $name)
    | toJson -}}
{{- end -}}

{{- define "project.namespace" -}}
{{- required "Set namespace." .Values.namespace -}}
{{- end -}}

{{- define "project.replicationCertificate" -}}
{{- printf "%s-client-repl" . -}}
{{- end -}}
