{{/*
Expand the name of the chart.
*/}}
{{- define "routed.name" -}}
{{- default .Chart.Name .Values.nameOverride | trunc 63 | trimSuffix "-" }}
{{- end }}

{{/*
Create a default fully qualified app name.
*/}}
{{- define "routed.fullname" -}}
{{- if .Values.fullnameOverride }}
{{- .Values.fullnameOverride | trunc 63 | trimSuffix "-" }}
{{- else }}
{{- $name := default .Chart.Name .Values.nameOverride }}
{{- if contains $name .Release.Name }}
{{- .Release.Name | trunc 63 | trimSuffix "-" }}
{{- else }}
{{- printf "%s-%s" .Release.Name $name | trunc 63 | trimSuffix "-" }}
{{- end }}
{{- end }}
{{- end }}

{{- define "routed.chart" -}}
{{- printf "%s-%s" .Chart.Name .Chart.Version | replace "+" "_" | trunc 63 | trimSuffix "-" }}
{{- end }}

{{- define "routed.labels" -}}
helm.sh/chart: {{ include "routed.chart" . }}
{{ include "routed.selectorLabels" . }}
{{- if .Chart.AppVersion }}
app.kubernetes.io/version: {{ .Chart.AppVersion | quote }}
{{- end }}
app.kubernetes.io/managed-by: {{ .Release.Service }}
app.kubernetes.io/part-of: routed
{{- end }}

{{- define "routed.selectorLabels" -}}
app.kubernetes.io/name: {{ include "routed.name" . }}
app.kubernetes.io/instance: {{ .Release.Name }}
app.kubernetes.io/component: router
{{- end }}

{{- define "routed.operator.selectorLabels" -}}
app.kubernetes.io/name: {{ include "routed.name" . }}-operator
app.kubernetes.io/instance: {{ .Release.Name }}
app.kubernetes.io/component: operator
{{- end }}

{{- define "routed.serviceAccountName" -}}
{{- if .Values.serviceAccount.create }}
{{- default (include "routed.fullname" .) .Values.serviceAccount.name }}
{{- else }}
{{- default "default" .Values.serviceAccount.name }}
{{- end }}
{{- end }}

{{- define "routed.image" -}}
{{- printf "%s:%s" .Values.image.repository (default .Chart.AppVersion .Values.image.tag) }}
{{- end }}

{{- define "routed.operator.image" -}}
{{- printf "%s:%s" .Values.operator.image.repository (default .Chart.AppVersion .Values.operator.image.tag) }}
{{- end }}

{{- define "routed.operator.fullname" -}}
{{- printf "%s-operator" (include "routed.fullname" .) | trunc 63 | trimSuffix "-" }}
{{- end }}

{{- define "routed.operator.labels" -}}
helm.sh/chart: {{ include "routed.chart" . }}
{{ include "routed.operator.selectorLabels" . }}
{{- if .Chart.AppVersion }}
app.kubernetes.io/version: {{ .Chart.AppVersion | quote }}
{{- end }}
app.kubernetes.io/managed-by: {{ .Release.Service }}
app.kubernetes.io/part-of: routed
{{- end }}

{{- define "routed.operator.serviceAccountName" -}}
{{- if .Values.operator.serviceAccount.create }}
{{- default (include "routed.operator.fullname" .) .Values.operator.serviceAccount.name }}
{{- else }}
{{- default "default" .Values.operator.serviceAccount.name }}
{{- end }}
{{- end }}
