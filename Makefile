# SPDX-License-Identifier: Apache-2.0
#
# routeD build entry points. This environment has no host Rust toolchain: every
# cargo invocation runs inside the pinned toolchain container via
# scripts/cargo-in-podman.sh. CI overrides CARGO=cargo to use a real toolchain.
# GNU Make 3.81 compatible (no .ONESHELL, no != assignments).

CARGO ?= ./scripts/cargo-in-podman.sh cargo
HELM  ?= helm
ROUTED_COMMIT ?= $(shell git rev-parse --short=12 HEAD 2>/dev/null || echo unknown)
export ROUTED_COMMIT
IMAGE_REPO ?= ghcr.io/vibed-project
IMAGE_TAG  ?= dev

.PHONY: all build build-release test onnx sbom fmt fmt-check clippy lint deny spdx boundary hygiene \
        crd-gen crd-check helm-lint image image-operator toolchain-image e2e e2e-up e2e-down ci clean

all: build

## Build
build:
	$(CARGO) build --workspace --locked

build-release:
	$(CARGO) build --workspace --release --locked

## Test
test:
	$(CARGO) test --workspace --locked

## Code generation (CRDs are derived from crates/api and committed; CI checks for drift)
crd-gen:
	$(CARGO) run -q -p routedctl -- crd gen --out config/crd
	rm -rf charts/routed/crds && mkdir -p charts/routed/crds && cp config/crd/*.yaml charts/routed/crds/
	$(CARGO) run -q -p routedctl -- crd docs --out docs/crds.md

crd-check: crd-gen
	git diff --exit-code -- config/crd charts/routed/crds docs/crds.md

## ONNX feature (not part of the default ci chain). The `onnx` build uses
## ort's load-dynamic mode (ADR-0002 fallback 1): this target downloads the
## Microsoft-published shared library into .cache/ and points ORT_DYLIB_PATH
## at it inside the toolchain container.
ORT_VERSION := 1.28.0
ORT_ARCH := $(shell uname -m | sed -e 's/^arm64$$/aarch64/' -e 's/^x86_64$$/x64/')
ORT_DIR := .cache/onnxruntime/onnxruntime-linux-$(ORT_ARCH)-$(ORT_VERSION)
ORT_SO := $(ORT_DIR)/lib/libonnxruntime.so

$(ORT_SO):
	mkdir -p .cache/onnxruntime
	curl -fsSL -o .cache/onnxruntime/ort.tgz \
	  https://github.com/microsoft/onnxruntime/releases/download/v$(ORT_VERSION)/onnxruntime-linux-$(ORT_ARCH)-$(ORT_VERSION).tgz
	tar -xzf .cache/onnxruntime/ort.tgz -C .cache/onnxruntime
	rm -f .cache/onnxruntime/ort.tgz

onnx: $(ORT_SO)
	ORT_DYLIB_PATH=/src/$(ORT_DIR)/lib/libonnxruntime.so $(CARGO) clippy -p routed-classify --features onnx --all-targets --locked -- -D warnings
	ORT_DYLIB_PATH=/src/$(ORT_DIR)/lib/libonnxruntime.so $(CARGO) test -p routed-classify --features onnx --locked

## SBOM (CycloneDX, one per binary crate; ADR-0019)
sbom:
	rm -rf sbom && mkdir -p sbom
	$(CARGO) cyclonedx --format json --spec-version 1.5
	for f in cmd/routed/routed.cdx.json cmd/routed-operator/routed-operator.cdx.json cmd/routedctl/routedctl.cdx.json; do \
	  [ -f "$$f" ] && mv "$$f" sbom/ || true; done
	find . -name '*.cdx.json' -not -path './sbom/*' -delete
	ls -la sbom/

## Lint
fmt:
	$(CARGO) fmt --all

fmt-check:
	$(CARGO) fmt --all -- --check

clippy:
	$(CARGO) clippy --workspace --all-targets --locked -- -D warnings

lint: fmt-check clippy

deny:
	$(CARGO) deny --all-features check

spdx:
	./scripts/check-spdx.sh

boundary:
	./scripts/check-crate-boundary.sh

hygiene: spdx boundary
	./scripts/check-hygiene.sh

helm-lint:
	$(HELM) lint charts/routed
	$(HELM) template routed charts/routed --set operator.enabled=true --set mode=extproc > /dev/null

## Images (local builds are single-arch for the podman machine)
image:
	podman build -f build/routed.Dockerfile --build-arg COMMIT=$(ROUTED_COMMIT) -t $(IMAGE_REPO)/routed:$(IMAGE_TAG) .

image-operator:
	podman build -f build/routed-operator.Dockerfile --build-arg COMMIT=$(ROUTED_COMMIT) -t $(IMAGE_REPO)/routed-operator:$(IMAGE_TAG) .

toolchain-image:
	podman build -t localhost/routed-toolchain:$$(sed -n 's/^channel *= *"\(.*\)"/\1/p' rust-toolchain.toml) -f build/toolchain.Containerfile build

## kind end-to-end (cluster routed-e2e; never touches other clusters)
e2e:
	./test/e2e/run.sh all

e2e-up:
	./test/e2e/run.sh up

e2e-down:
	./test/e2e/run.sh down

## CI mirror
ci: hygiene lint test deny crd-check helm-lint

clean:
	rm -rf target/ bin/
