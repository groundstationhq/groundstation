# The backend services (ingest, api, worker) in one image, with the UI built
# and embedded in api. Pick the service with the command:
#   docker run ... groundstation-server groundstation-api
# Configuration is environment variables only; see docs/backend.md.

# Base images are pinned by digest, so a commit always builds the same image;
# Dependabot proposes updates.
FROM node:26.13.0-trixie-slim@sha256:5adad64f9a1fca2d51972caa65983634c8a774eb300d72b20f7f2a0f60368072 AS ui
WORKDIR /src/ui
COPY ui/package.json ui/package-lock.json ./
RUN npm ci --no-audit --no-fund
COPY ui/ ./
RUN npm run build

FROM rust:1.98.1-trixie@sha256:a8a5f0a1e5fe7dfe1d352591e4a1c7dd2c08fd70475cae872cf3458ba0df0546 AS build
WORKDIR /src
COPY . .
COPY --from=ui /src/ui/dist ui/dist
# Fail rather than embed the placeholder page if the UI build is missing.
ENV GS_UI_REQUIRED=1
# Only the registry is cached: a cached target/ can keep stale builds, since
# file times in the build context aren't reliable.
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    cargo build --release --locked -p groundstation-ingest -p groundstation-api -p groundstation-worker \
    && mkdir /out \
    && cp target/release/groundstation-ingest target/release/groundstation-api target/release/groundstation-worker /out/

FROM gcr.io/distroless/cc-debian13:nonroot@sha256:e792ab3d241a468a4fd7519ddbbebe66b49b5f365771716ea688ad40b6c6f1c2
COPY --from=build /out/ /usr/local/bin/
COPY LICENSE NOTICE /usr/share/doc/groundstation/
ENV GROUNDSTATION_INGEST_LISTEN=0.0.0.0:4320 \
    GROUNDSTATION_API_LISTEN=0.0.0.0:4321
EXPOSE 4320 4321
CMD ["groundstation-api"]
