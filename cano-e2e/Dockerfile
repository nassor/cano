# Image for the `cano_workflow_app` binary — the "custom software" the end-to-end
# tests run in a container alongside a Postgres container.
#
# Build context MUST be the workspace root (it needs `cano/`, `cano-macros/`,
# `cano-e2e/` and `Cargo.lock`):
#
#   docker build -t cano-workflow-app:e2e -f cano-e2e/Dockerfile .

FROM rust:1.95-slim AS build
WORKDIR /src
COPY . .
RUN cargo build --release -p cano-e2e --bin cano_workflow_app

FROM debian:bookworm-slim
RUN useradd --create-home --user-group app
COPY --from=build /src/target/release/cano_workflow_app /usr/local/bin/cano_workflow_app
USER app
ENTRYPOINT ["cano_workflow_app"]
