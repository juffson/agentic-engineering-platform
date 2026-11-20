# syntax=docker/dockerfile:1
ARG RUST_IMAGE=rust:1-bookworm
ARG RUNTIME_IMAGE=debian:bookworm-slim
ARG NODE_IMAGE=node:24-bookworm-slim

FROM ${RUST_IMAGE} AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN cargo build --locked --bin aep --jobs 2

FROM ${RUNTIME_IMAGE} AS control
COPY --from=build /src/target/debug/aep /usr/local/bin/aep
COPY deploy/pilot/control.sh /usr/local/bin/aep-pilot-control
USER 65532:65532
ENTRYPOINT ["/usr/local/bin/aep-pilot-control"]

FROM ${NODE_IMAGE} AS clients
ARG CODEX_VERSION=0.162.1
ARG CLAUDE_VERSION=2.1.327
RUN npm install -g "@openai/codex@${CODEX_VERSION}" "@anthropic-ai/claude-code@${CLAUDE_VERSION}" && \
    codex --version && claude --version && rm -rf /root/.npm

FROM ${RUST_IMAGE} AS worker
RUN apt-get update && apt-get install -y --no-install-recommends curl util-linux && \
    groupadd --gid 10001 aep-agent && \
    useradd --uid 10001 --gid 10001 --home-dir /pilot/agent --no-create-home aep-agent && \
    rm -rf /var/lib/apt/lists/*
COPY --from=clients /usr/local/ /usr/local/
COPY --from=build /usr/local/cargo/registry /opt/aep-cargo/registry
COPY --from=build /src/target/debug/aep /usr/local/bin/aep
COPY deploy/pilot/worker.sh deploy/pilot/sandbox.sh /usr/local/libexec/aep-pilot/
COPY deploy/pilot/supervisor-git.sh /usr/local/libexec/aep-pilot/git
COPY deploy/pilot/codex-container.sh /usr/local/libexec/aep-executor/codex
RUN rustup component add rustfmt clippy && touch /etc/aep-pilot-container && \
    for tool in cargo rustc rustfmt cargo-fmt cargo-clippy clippy-driver rustup; do \
      ln -s "/usr/local/cargo/bin/$tool" "/usr/local/bin/$tool"; \
    done
WORKDIR /pilot
ENTRYPOINT ["/usr/local/libexec/aep-pilot/worker.sh"]
