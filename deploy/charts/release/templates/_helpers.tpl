{{/* The annotations every object of this chart carries. */}}
{{- define "release.annotations" -}}
wamn.environment: {{ required "environment is required" .Values.environment | quote }}
wamn.release-digest: {{ required "release.manifestDigest is required" .Values.release.manifestDigest | quote }}
{{- with .Values.applications }}
{{- if ne (len .) 1 }}{{ fail "a release must select exactly one application" }}{{ end }}
wamn.application: {{ keys . | first | quote }}
{{- end }}
{{- end }}

{{/* Application workloads share their environment host. */}}
{{- define "release.hostSelector" -}}
hostgroup: {{ required "hostGroup is required" .Values.hostGroup | quote }}
{{- end }}

{{/* Refuse a chart whose HTTP image was not selected. */}}
{{- define "release.requireImageSet" -}}
{{- $_ := required "platform.roles.http.image is required: stamp the platform image set" .Values.platform.roles.http.image }}
{{- end }}
