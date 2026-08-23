# syntax=docker/dockerfile:1.7
# routed-operator image: multi-stage, release build, distroless runtime.
# Builder and runtime share Debian 12 (glibc 2.36); keep them in lockstep.
FROM docker.io/library/rust:1.92-bookworm AS build
WORKDIR /src
ARG COMMIT=unknown
ENV ROUTED_COMMIT=${COMMIT} CARGO_TERM_COLOR=always
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked -p routed-operator && cp target/release/routed-operator /out-routed-operator

FROM gcr.io/distroless/cc-debian12:nonroot
COPY --from=build /out-routed-operator /usr/local/bin/routed-operator
EXPOSE 8080 8081 9090
USER nonroot:nonroot
ENTRYPOINT ["/usr/local/bin/routed-operator"]

