{{- define "org.controlHost" -}}
{{- printf "%s.%s" .Values.org (required "Set base to the platform domain." .Values.base) -}}
{{- end -}}
