{{- define "env.namespace" -}}
{{- required "Set namespace." .Values.namespace -}}
{{- end -}}


{{- define "env.httpPort" -}}
9191
{{- end -}}
