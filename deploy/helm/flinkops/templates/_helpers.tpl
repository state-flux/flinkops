{{/*
镜像引用：image.tag 留空时用 Chart.appVersion，避免"chart 升了 tag 没升"。
*/}}
{{- define "flinkops.image" -}}
{{- $tag := default .Chart.AppVersion .Values.image.tag -}}
{{- printf "%s:%s" .Values.image.repository $tag -}}
{{- end -}}

{{/*
flinkops.fullname：release 名 + chart 名，超过 63 字符则截断
*/}}
{{- define "flinkops.fullname" -}}
{{- $name := default .Chart.Name .Values.nameOverride -}}
{{- if contains $name .Release.Name -}}
{{- .Release.Name | trunc 63 | trimSuffix "-" -}}
{{- else -}}
{{- printf "%s-%s" .Release.Name $name | trunc 63 | trimSuffix "-" -}}
{{- end -}}
{{- end -}}
