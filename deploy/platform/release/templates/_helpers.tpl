{{/* The annotations every object of this chart carries. */}}
{{- define "release.annotations" -}}
wamn.environment: {{ required "environment is required" .Values.environment | quote }}
wamn.release-digest: {{ required "release.manifestDigest is required" .Values.release.manifestDigest | quote }}
{{- end }}

{{/* The selector of every role workload: the host group and the release.
     The release is the `wamn.release` pod label the renderer wrote; the chart
     carries no second copy of it. */}}
{{- define "release.hostSelector" -}}
{{- $podLabels := index .Values "runtime-operator" "runtime" "podLabels" | default dict }}
hostgroup: {{ .Release.Name | quote }}
wamn.release: {{ required "runtime-operator.runtime.podLabels.wamn.release is required" (index $podLabels "wamn.release") | quote }}
{{- end }}

{{/* Refuse a chart whose platform image set was not stamped. */}}
{{- define "release.requireImageSet" -}}
{{- $image := index .Values "runtime-operator" "runtime" "image" }}
{{- $_ := required "runtime-operator.runtime.image.repository is required: stamp the platform image set" $image.repository }}
{{- $_ := required "runtime-operator.runtime.image.tag is required: stamp the platform image set" $image.tag }}
{{- $_ := required "platform.roles.http.image is required: stamp the platform image set" .Values.platform.roles.http.image }}
{{- $_ := required "platform.roles.materializer.image is required: stamp the platform image set" .Values.platform.roles.materializer.image }}
{{- end }}
