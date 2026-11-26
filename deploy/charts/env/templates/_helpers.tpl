{{- define "env.namespace" -}}
{{- required "Set org." .Values.org -}}
{{- end -}}


{{- define "env.httpPort" -}}
9191
{{- end -}}
