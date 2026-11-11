{{/* Names: short enough that "-clickhouse" still fits in a DNS label. */}}
{{- define "groundstation.name" -}}
{{- default .Chart.Name .Values.nameOverride | trunc 50 | trimSuffix "-" }}
{{- end }}

{{- define "groundstation.fullname" -}}
{{- if .Values.fullnameOverride }}
{{- .Values.fullnameOverride | trunc 50 | trimSuffix "-" }}
{{- else }}
{{- $name := default .Chart.Name .Values.nameOverride }}
{{- if contains $name .Release.Name }}
{{- .Release.Name | trunc 50 | trimSuffix "-" }}
{{- else }}
{{- printf "%s-%s" .Release.Name $name | trunc 50 | trimSuffix "-" }}
{{- end }}
{{- end }}
{{- end }}

{{/* Labels of one component: (dict "root" . "component" "api"). */}}
{{- define "groundstation.selectorLabels" -}}
app.kubernetes.io/name: {{ include "groundstation.name" .root }}
app.kubernetes.io/instance: {{ .root.Release.Name }}
app.kubernetes.io/component: {{ .component }}
{{- end }}

{{- define "groundstation.labels" -}}
helm.sh/chart: {{ printf "%s-%s" .root.Chart.Name .root.Chart.Version | replace "+" "_" | trunc 63 | trimSuffix "-" }}
{{ include "groundstation.selectorLabels" . }}
app.kubernetes.io/version: {{ .root.Chart.AppVersion | quote }}
app.kubernetes.io/part-of: groundstation
app.kubernetes.io/managed-by: {{ .root.Release.Service }}
{{- end }}

{{/* The backend's image, one for ingest, api and worker. */}}
{{- define "groundstation.image" -}}
{{- $repository := required "image.repository is required" .Values.image.repository }}
{{- if .Values.image.digest }}
{{- printf "%s@%s" $repository .Values.image.digest }}
{{- else }}
{{- printf "%s:%s" $repository (.Values.image.tag | default .Chart.AppVersion) }}
{{- end }}
{{- end }}

{{- define "groundstation.secretName" -}}
{{- .Values.secrets.existingSecret | default (include "groundstation.fullname" .) }}
{{- end }}

{{/* Changes when a secret value set in the values does, so pods restart with it. */}}
{{- define "groundstation.secretChecksum" -}}
{{- dict "secrets" .Values.secrets "admin" .Values.signIn.adminPassword "oidc" .Values.signIn.oidc.clientSecret | toJson | sha256sum }}
{{- end }}

{{/* Where the services are, for each other and for the edge. */}}
{{- define "groundstation.api.address" -}}
{{- printf "%s-api:4321" (include "groundstation.fullname" .) }}
{{- end }}

{{- define "groundstation.ingest.address" -}}
{{- printf "%s-ingest:%v" (include "groundstation.fullname" .) .Values.ingest.service.port }}
{{- end }}

{{/*
ClickHouse settings of one service: (dict "root" . "service" "api"), where
service is ingest, api or worker. Each connects as its own user, gs_<service>,
which the services default to.
*/}}
{{- define "groundstation.env.clickhouse" -}}
{{- $root := .root }}
- name: GROUNDSTATION_CLICKHOUSE_URL
{{- if $root.Values.clickhouse.enabled }}
  value: {{ printf "http://%s-clickhouse:8123" (include "groundstation.fullname" $root) | quote }}
{{- else }}
  value: {{ required "externalClickhouse.url is required when clickhouse.enabled is false" $root.Values.externalClickhouse.url | quote }}
{{- end }}
- name: GROUNDSTATION_CLICKHOUSE_DATABASE
  value: {{ include "groundstation.clickhouse.database" $root | quote }}
- name: GROUNDSTATION_CLICKHOUSE_PASSWORD
  valueFrom:
    secretKeyRef:
      name: {{ include "groundstation.secretName" $root }}
      key: clickhouse-{{ .service }}-password
{{- end }}

{{- define "groundstation.clickhouse.database" -}}
{{- if .Values.clickhouse.enabled }}groundstation{{ else }}{{ .Values.externalClickhouse.database }}{{ end }}
{{- end }}

{{- define "groundstation.clickhouse.adminUser" -}}
{{- if .Values.clickhouse.enabled }}groundstation{{ else }}{{ .Values.externalClickhouse.adminUser }}{{ end }}
{{- end }}

{{/*
The Postgres URL of one service: (dict "root" . "role" "ingest"), where role
is owner (the api, which migrates), ingest or worker. The password comes from
the Secret through a variable Kubernetes expands inside the URL.
*/}}
{{- define "groundstation.env.postgres" -}}
{{- $root := .root }}
{{- $external := $root.Values.externalPostgres }}
{{- $bundled := $root.Values.postgres.enabled }}
{{- $role := .role }}
{{- if and (not $bundled) (not $external.separateRoles) }}
{{- $role = "owner" }}
{{- end }}
{{- $user := printf "gs_%s" $role }}
{{- if eq $role "owner" }}
{{- $user = ternary "groundstation" $external.username $bundled }}
{{- end }}
{{- $host := printf "%s-postgres:5432" (include "groundstation.fullname" $root) }}
{{- $database := "groundstation" }}
{{- $params := "" }}
{{- if not $bundled }}
{{- $host = printf "%s:%v" (required "externalPostgres.host is required when postgres.enabled is false" $external.host) $external.port }}
{{- $database = $external.database }}
{{- $params = $external.params }}
{{- end }}
- name: GS_POSTGRES_PASSWORD
  valueFrom:
    secretKeyRef:
      name: {{ include "groundstation.secretName" $root }}
      key: postgres-{{ $role }}-password
- name: GROUNDSTATION_POSTGRES_URL
  value: {{ printf "postgres://%s:$(GS_POSTGRES_PASSWORD)@%s/%s%s" $user $host $database (ternary (printf "?%s" $params) "" (ne $params "")) | quote }}
{{- end }}

{{/* What every pod gets: no API token, and where it may run. */}}
{{- define "groundstation.podDefaults" -}}
automountServiceAccountToken: false
{{- with .Values.imagePullSecrets }}
imagePullSecrets:
  {{- toYaml . | nindent 2 }}
{{- end }}
{{- with .Values.nodeSelector }}
nodeSelector:
  {{- toYaml . | nindent 2 }}
{{- end }}
{{- with .Values.tolerations }}
tolerations:
  {{- toYaml . | nindent 2 }}
{{- end }}
{{- with .Values.affinity }}
affinity:
  {{- toYaml . | nindent 2 }}
{{- end }}
{{- end }}

{{/* For containers that need nothing: the backend's (distroless, nonroot), the edge, the waits. */}}
{{- define "groundstation.restricted" -}}
allowPrivilegeEscalation: false
readOnlyRootFilesystem: true
runAsNonRoot: true
runAsUser: 65532
runAsGroup: 65532
capabilities:
  drop: [ALL]
seccompProfile:
  type: RuntimeDefault
{{- end }}

{{/*
An init container that waits for addresses to accept connections:
(dict "root" . "name" "wait-for-databases" "addresses" (list "host:port") "seconds" 300).
It gives up after `seconds` and lets the service start anyway: what it waits
for makes a start clean, and must not keep a service down for good.
*/}}
{{- define "groundstation.wait" -}}
- name: {{ .name }}
  image: {{ printf "%s:%s" .root.Values.waitImage.repository .root.Values.waitImage.tag | quote }}
  imagePullPolicy: {{ .root.Values.waitImage.pullPolicy }}
  command:
    - sh
    - -c
    - |
      end=$(( $(date +%s) + {{ .seconds }} ))
      for address in {{ join " " .addresses }}; do
        until nc -z -w 2 "${address%:*}" "${address##*:}"; do
          if [ "$(date +%s)" -ge "$end" ]; then
            echo "gave up waiting for $address"
            exit 0
          fi
          echo "waiting for $address"
          sleep 2
        done
      done
  securityContext:
    {{- include "groundstation.restricted" .root | nindent 4 }}
{{- end }}
