{{- define "org.controlHost" -}}
{{- printf "%s.%s" .Values.org (required "Set base to the platform wildcard base." .Values.base) -}}
{{- end -}}
