# syntax=docker/dockerfile:1.7
# routed-mockgateway image (tests only): multi-stage, release build, distroless runtime.
FROM docker.io/library/rust:1.97-bookworm AS build
WORKDIR /src
ARG COMMIT=unknown
ENV ROUTED_COMMIT=${COMMIT} CARGO_TERM_COLOR=always
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked -p routed-mockgateway && cp target/release/routed-mockgateway /out-routed-mockgateway

FROM gcr.io/distroless/cc-debian12:nonroot
COPY --from=build /out-routed-mockgateway /usr/local/bin/routed-mockgateway
EXPOSE 4000
USER nonroot:nonroot
ENTRYPOINT ["/usr/local/bin/routed-mockgateway"]
