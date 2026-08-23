# syntax=docker/dockerfile:1.7
# routed image: multi-stage, release build, distroless runtime.
# Builder and runtime share Debian 12 (glibc 2.36); keep them in lockstep.
FROM docker.io/library/rust:1.92-bookworm AS build
WORKDIR /src
ARG COMMIT=unknown
ENV ROUTED_COMMIT=${COMMIT} CARGO_TERM_COLOR=always
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked -p routed && cp target/release/routed /out-routed

FROM gcr.io/distroless/cc-debian12:nonroot
COPY --from=build /out-routed /usr/local/bin/routed
EXPOSE 8080 9002
USER nonroot:nonroot
ENTRYPOINT ["/usr/local/bin/routed"]
CMD ["serve", "--mode", "inline"]
