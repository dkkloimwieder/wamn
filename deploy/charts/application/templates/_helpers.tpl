{{/* The annotations every object of this chart carries. */}}
{{- define "application.annotations" -}}
wamn.environment: {{ required "environment is required" .Values.environment | quote }}
{{- $_ := required "application.manifestDigest is required" .Values.release.manifestDigest }}
{{- with .Values.applications }}
{{- if ne (len .) 1 }}{{ fail "a release must select exactly one application" }}{{ end }}
wamn.application: {{ keys . | first | quote }}
{{- end }}
{{- end }}

{{/* Application workloads share their environment host. */}}
{{- define "application.hostSelector" -}}
hostgroup: {{ required "hostGroup is required" .Values.hostGroup | quote }}
{{- end }}

{{/* Require both application workload guests. */}}
{{- define "application.requireImageSet" -}}
{{- $_ := required "platform.roles.ingress.image is required: stamp the platform image set" .Values.platform.roles.ingress.image }}
{{- $_ := required "platform.workflow.image is required: stamp the platform image set" .Values.platform.workflow.image }}
{{- end }}

{{- define "application.namespace" -}}
{{- required "Set namespace." .Values.namespace -}}
{{- end -}}
