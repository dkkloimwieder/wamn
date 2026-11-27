{{/* The annotations every object of this chart carries. */}}
{{- define "app.annotations" -}}
wamn.env: {{ required "env is required" .Values.env | quote }}
{{- $_ := required "release.manifestDigest is required" .Values.release.manifestDigest }}
{{- with .Values.apps }}
{{- if ne (len .) 1 }}{{ fail "a release must select exactly one application" }}{{ end }}
wamn.app: {{ keys . | first | quote }}
{{- end }}
{{- end }}

{{/* The route workload runs on the shared environment host. */}}
{{- define "app.hostSelector" -}}
hostgroup: {{ required "hostGroup is required" .Values.hostGroup | quote }}
{{- end }}

{{/* Require both guests of the route workload. */}}
{{- define "app.requireImageSet" -}}
{{- $_ := required "platform.roles.ingress.image is required: stamp the platform image set" .Values.platform.roles.ingress.image }}
{{- $_ := required "platform.workflow.image is required: stamp the platform image set" .Values.platform.workflow.image }}
{{- end }}

{{- define "app.namespace" -}}
{{- required "Set namespace." .Values.namespace -}}
{{- end -}}

{{- define "app.httpService" -}}
{{- printf "%s-http" (default .Release.Name .Values.name) -}}
{{- end -}}
