# syntax=docker/dockerfile:1.7
# Local development toolchain image for routeD.
# The official rust image ships the "minimal" rustup profile (no clippy/rustfmt);
# this layer adds them plus cargo-deny so `make lint deny` works without a host toolchain.
FROM docker.io/library/rust:1.92-bookworm
ARG CARGO_DENY_VERSION=0.20.2
ARG CARGO_CYCLONEDX_VERSION=0.5.7
ARG TARGETARCH
RUN rustup component add clippy rustfmt
RUN set -eux; \
    case "${TARGETARCH:-$(dpkg --print-architecture)}" in \
      amd64) a=x86_64 ;; \
      arm64) a=aarch64 ;; \
      *) echo "unsupported arch ${TARGETARCH}"; exit 1 ;; \
    esac; \
    curl -fsSL "https://github.com/EmbarkStudios/cargo-deny/releases/download/${CARGO_DENY_VERSION}/cargo-deny-${CARGO_DENY_VERSION}-${a}-unknown-linux-musl.tar.gz" \
      | tar -xz --strip-components=1 -C /usr/local/cargo/bin "cargo-deny-${CARGO_DENY_VERSION}-${a}-unknown-linux-musl/cargo-deny"; \
    cargo deny --version
# SBOM generation (ADR-0019); built from source, no prebuilt binaries published.
RUN cargo install --locked cargo-cyclonedx --version "${CARGO_CYCLONEDX_VERSION}" \
    && cargo cyclonedx --version && rm -rf /usr/local/cargo/registry
ENV CARGO_TERM_COLOR=always
