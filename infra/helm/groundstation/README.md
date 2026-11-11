# Ground Station on Kubernetes

A Helm chart for the backend `gsd` uploads to: everything `docker compose --profile selfhost` runs, as one release.

| Component | What it is |
|:--|:--|
| `api` | The UI, the `/v1` reads and sign-in. Migrates both databases when it starts |
| `ingest` | Takes uploads from `gsd` on `POST /v1/events`. Can have a host name of its own |
| `worker` | Runs the detectors over each tenant's recent events |
| `edge` | Caddy, one origin for all of it: uploads go to ingest, the rest to api. Its Service is what you expose |
| `clickhouse` | Events. One server with a volume; turn it off to use your own |
| `postgres` | Tenants, users, tokens and findings. One server with a volume; turn it off to use your own |

[`docs/backend.md`](../../../docs/backend.md) describes the services and every setting they take. Needs Kubernetes 1.25 or later and Helm 3 or 4.

## Install

From the repository:

```sh
helm install groundstation infra/helm/groundstation \
  --namespace groundstation --create-namespace
```

That runs the published image, `ghcr.io/groundstationhq/groundstation-server`, which is public and built for amd64 and arm64. Every release publishes one under its version, and the chart at that release installs it. To run your own build instead, push it where your cluster pulls from and set `image.repository` and `image.tag`:

```sh
docker buildx build --platform linux/amd64,linux/arm64 --tag registry.example.com/groundstation-server:dev --push .
```

The notes it prints say how to reach it and how to read the admin password it generated. With nothing else set, the edge has no address outside the cluster and `publicUrl` is `http://127.0.0.1:8080`, which is what a port-forward serves:

```sh
kubectl --namespace groundstation port-forward svc/groundstation-edge 8080:80
groundstation cloud login --endpoint http://127.0.0.1:8080
```

`helm test groundstation --namespace groundstation` checks that both services reach their databases and that the edge routes to them.

## Giving it an address

`publicUrl` is the origin people and `gsd` use: scheme, host and optional port. Sign-in redirects, cookies and same-origin checks are based on it, so it must be exactly what the browser's address bar shows, or sign-in is refused. Pick the address first, then set both.

A load balancer address, for a cluster with MetalLB, Cilium or a cloud's:

```yaml
publicUrl: http://192.168.1.50
edge:
  service:
    type: LoadBalancer
    annotations:
      lbipam.cilium.io/ips: 192.168.1.50 # or metallb.io/loadBalancerIPs
```

An Ingress, for a host name and TLS:

```yaml
publicUrl: https://gs.example.com
ingress:
  enabled: true
  className: nginx
  host: gs.example.com
  tlsSecretName: gs-example-com-tls
  annotations:
    nginx.ingress.kubernetes.io/proxy-body-size: 8m
```

Whatever is in front of the edge must pass `Origin` and `Cookie` through unchanged, take request bodies of at least 8 MiB on `POST /v1/events` and wait at least 60 seconds for one. Over plain http the UI works, without `Secure` cookies, but `gsd` uploads only over https, or to `localhost`: its token and the events would otherwise cross the network unencrypted. So an http address on your network is enough to look around, and uploading from other computers needs https in front. To upload from one computer without it, forward the edge to that computer and give `gsd` a token made on the Tokens page:

```sh
kubectl --namespace groundstation port-forward svc/groundstation-edge 8080:80
groundstation cloud login --endpoint http://127.0.0.1:8080 --token-stdin
```

## Uploads on their own host name

By default everything shares `publicUrl`, and the edge sends `POST /v1/events` to ingest and the rest to the api. To keep the two apart, give uploads an origin of their own:

```yaml
publicUrl: https://app.example.com
ingestUrl: https://ingest.example.com
ingress:
  enabled: true
  className: nginx
  host: app.example.com
  tlsSecretName: app-example-com-tls
  ingest:
    host: ingest.example.com
    tlsSecretName: ingest-example-com-tls
    annotations:
      nginx.ingress.kubernetes.io/proxy-body-size: 8m
      nginx.ingress.kubernetes.io/proxy-read-timeout: "60"
```

Uploads then go straight to ingest, through an Ingress of its own that routes `POST /v1/events` and nothing else; they pass neither the edge nor the api. Ingest and the web can be scaled, rate-limited and restarted separately (`ingest.replicas`, the two sets of annotations), and one being down doesn't take the other's address with it. They still share ClickHouse and Postgres.

People don't need to know the second address. They run `groundstation cloud login --endpoint <publicUrl>` as the UI tells them, and the api tells the CLI where uploads go before it says what will be uploaded. That needs a `groundstation` CLI and an api that know `GROUNDSTATION_INGEST_URL`; an older CLI uploads to `publicUrl`, which keeps working through the edge.

Without an Ingress, `ingest.service` gives ingest a LoadBalancer or NodePort of its own; put TLS in front of it, since `gsd` uploads only over https.

## Signing in

The admin password is on by default and generated at the first install. For OpenID Connect:

```yaml
signIn:
  adminPassword:
    enabled: false
  oidc:
    issuer: https://accounts.google.com
    clientId: ...
    clientSecret: ...
    name: Google
    allowedDomains: [example.com]
```

Register `<publicUrl>/auth/callback` as the redirect URL at the provider. New people join one shared tenant (`signIn.newUsers: shared`); with a provider anyone can get an account at, set `allowedDomains`.

## Passwords

The release keeps its passwords in one Secret, named after the release. Each value you don't set is generated at the first install and never changes by itself: it survives upgrades, and the Secret stays when the release is uninstalled, like the databases' volumes, so a reinstall under the same name still opens them.

| Key | For |
|:--|:--|
| `admin-password` | Signing in as the admin (unless `signIn.adminPassword.enabled` is false) |
| `oidc-client-secret` | The identity provider (only with `signIn.oidc`) |
| `clickhouse-admin-password` | The ClickHouse user the api migrates as |
| `clickhouse-ingest-password`, `clickhouse-api-password`, `clickhouse-worker-password` | Each service's own ClickHouse user |
| `postgres-owner-password` | The Postgres owner, which the api connects as |
| `postgres-ingest-password`, `postgres-worker-password` | The Postgres roles of ingest and the worker |

Ingest and the worker get only their own credentials, so if one is broken into it can't act as another.

To manage the Secret yourself, create one with these keys and set `secrets.existingSecret`. Do that with tools that render charts without reading the cluster (Argo CD, `helm template | kubectl apply`): they can't see the generated values, so they would make new ones at every render. Use letters and digits; the Postgres passwords go into a connection URL.

The databases take their passwords when their volumes are first created. Changing one later means changing it in the database too: `docs/backend.md` has the statements, to run with `kubectl exec` in the `clickhouse-0` or `postgres-0` pod.

## Your own databases

```yaml
clickhouse:
  enabled: false
externalClickhouse:
  url: https://abc123.eu-central-1.aws.clickhouse.cloud:8443
  adminUser: default
postgres:
  enabled: false
externalPostgres:
  host: db.example.com
  params: sslmode=require
```

Put their passwords under `secrets` (or in your own Secret). ClickHouse needs the users and roles [`docker/clickhouse/users.sh`](../../../docker/clickhouse/users.sh) creates, and Postgres the roles [`docker/postgres/roles.sh`](../../../docker/postgres/roles.sh) creates, unless `externalPostgres.separateRoles` is false, which has all three services connect as the owner. The api grants the roles their privileges when it migrates.

## Upgrading and removing

Upgrading is a new image, and the api migrates when it starts. The chart at each release installs that release's image, so check out the release and `helm upgrade`; or stay on the chart you have and set `image.tag`.

```sh
helm uninstall groundstation --namespace groundstation
```

removes the services and leaves the data: the Secret and the two volume claims. To delete those too:

```sh
kubectl --namespace groundstation delete secret groundstation
kubectl --namespace groundstation delete pvc data-groundstation-clickhouse-0 data-groundstation-postgres-0
```

Delete all three or none: a new Secret doesn't open an old volume.

## What the bundled databases are not

One ClickHouse server and one Postgres server, each on one volume, with no replication and no backups. That is right for a homelab, a team's own instance or a trial. For anything you can't lose, run the databases the way you run your others and point the chart at them.

## Working on the chart

`values.yaml` documents every value. `files/` holds links to the edge's Caddyfile and the databases' setup scripts under `docker/`, so Kubernetes and Docker Compose run the same ones; Helm follows the links, also when packaging.

```sh
helm lint infra/helm/groundstation -f infra/helm/groundstation/ci/default-values.yaml
helm lint infra/helm/groundstation -f infra/helm/groundstation/ci/external-values.yaml
helm template gs infra/helm/groundstation -f infra/helm/groundstation/ci/default-values.yaml
```
