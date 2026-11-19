{{- define "environment.namespace" -}}
{{- required "Set org." .Values.org -}}
{{- end -}}


{{- define "environment.httpPort" -}}
9191
{{- end -}}
