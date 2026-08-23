# Threat model

> v0.1.0. Review this document when a trust boundary changes; ADRs referenced
> throughout carry the detailed contracts.

## What routeD is trusted with

routeD decides which model tier serves a request. It never holds provider
credentials and never calls model providers (ADR-0001); the blast radius of
a fully compromised routeD is therefore **misrouting**: sending a request to
a cheaper / non-sovereign / lower-quality tier, failing to BLOCK, or denial
of service. That is serious (sovereignty violations are the point of the
product) but bounded: provider keys, billing and response content live in
the gateway.

Assets, most valuable first:

1. **Routing integrity**: the guarantee that hard constraints (data class,
   jurisdiction, CLOUD Act exposure, risk caps) are enforced before any
   optimisation (ADR-0003).
2. **Request content**: prompts pass through routeD in both ingress modes.
   They must never be persisted or logged (invariant; enforced by review,
   the feedback-journal test asserts no request content lands on disk).
3. **Routing configuration**: the four CRDs and the compiled snapshot.
4. **The decision record**: headers / spans / journal used for audit.

## Trust boundaries and threats

### 1. Caller -> router (untrusted)

Threats: header spoofing to reach a forbidden tier, prompt injection to
influence classification, oversized / malformed bodies, path smuggling.

Mitigations: `X-Routed-*` request headers are untrusted and can only
tighten a decision; policy override requires `overridable: true`
(ADR-0007). Inbound `x-routed-*` is stripped before anything is forwarded
in both modes (tested inline, ext_proc and e2e). Bodies are size-capped
before parsing; unparseable bodies fail closed (400, nothing forwarded).
Paths are normalised and dot-segment / encoded-separator smuggling is
rejected; only the `/v1` surface is forwarded by default. Classification
influences findings, but a classifier can only ever tighten what the
heuristics find (risk = max(model, heuristics), ADR-0016), and a degraded
classifier triggers the policy fallback, never a permissive guess
(ADR-0006).

Residual: a caller who can freely rephrase prompts can steer the *task*
classification (e.g. phrase everything as "chat"); policies must not treat
task labels as a security boundary. Data classes with header/PII detection
plus hard constraints are the security boundary.

### 2. Router -> gateway

Threats: routeD's rewrite being ignored; response tampering.

Mitigations: the gateway is the enforcement point by design; integration
guides require honouring the rewritten `model` and failing closed
(`failure_mode_allow: false`). Upstream responses cannot spoof decision
headers (`X-Routed-*` from upstream is stripped; tested). routeD does not
inspect responses; response integrity is the gateway/TLS story.

Residual: a malicious gateway defeats routing entirely. The gateway is
inside the trust base; choose and secure it accordingly.

### 3. Operator -> API server / router (control plane)

Threats: malicious or malformed CRDs; snapshot tampering in distribution;
a compromised operator writing arbitrary cluster state.

Mitigations: one pure compiler validates everything (ADR-0008); the
admission webhook rejects bad CRDs with attributed diagnostics, fail-open
by default so it cannot lock out remediation (ADR-0015). Snapshots are
content-hashed; the router logs the hash on every swap and each decision
records `snapshotHash` for audit. Operator RBAC is minimal (read CRDs,
write status, one ConfigMap, one Lease); the router has **no** Kubernetes
API access at all (ADR-0014). gRPC snapshot distribution is in-cluster
plaintext in v0.1.0 - see Gaps.

Residual / gaps: anyone with CRD write access controls routing; that is the
intended administrative boundary - protect it with Kubernetes RBAC. The
snapshot gRPC stream is unauthenticated inside the cluster; a NetworkPolicy
restricting the operator's 9090 to router pods is recommended until mTLS
lands.

### 4. Model artifacts (supply chain)

Threats: a swapped classifier or learned-router model silently loosening
routing; registry compromise; cache poisoning.

Mitigations: every remote artifact is digest-pinned and verified before
use, cache hits re-verify, mismatches never enter the cache (ADR-0016).
`oci://` pulls verify the manifest against the pinned digest and the layer
against the manifest (ADR-0019). A configured model that fails to load
stops startup rather than degrading silently. Structurally, models can
only tighten decisions: classifier risk is floored by the heuristics, and
learned-router predictions only refine `predictedQuality` inside the
engine after hard constraints.

Residual: digest pinning authenticates *content*, not *choice*: whoever
writes the RouterProfile chooses the model. That is again the CRD-write
administrative boundary.

### 5. Release artifacts

Threats: tampered images / chart; dependency compromise.

Mitigations: images are cosign-signed (keyless, CI identity) with CycloneDX
SBOMs; `cargo deny` gates advisories and licenses in CI; the toolchain and
runtime images are digest-pinned Debian releases in lockstep (ADR-0002,
ADR-0019). Builds are reproducible from a tag in CI only.

### 6. Telemetry, feedback and the learning loop

Threats: prompt leakage through logs / spans / journal; feedback poisoning
to degrade future routing; disk exhaustion.

Mitigations: prompts are never logged; the optional prompt hash is salted
and off by default. The decision journal carries closed-vocabulary labels
only (tested). Feedback never changes routing online (ADR-0018); poisoned
feedback can only degrade a *future, offline-trained* model, which ships
through the same review-and-pin pipeline as any artifact and still cannot
loosen hard constraints. Journal writes are bounded and drop-not-block.
Client-controlled strings are length-capped before reaching logs or spans.

Residual: feedback is unauthenticated beyond the platform's ingress
controls; rate limiting and caller auth are the gateway's job (ADR-0001).
Journal files grow unbounded on the emptyDir; rotate or ship them.

### 7. Denial of service

Body caps before parsing, classification behind a bounded semaphore with
strict timeouts, streaming idle watchdogs, bounded decision-header size,
and fail-closed readiness. routeD inherits the platform's L4/L7 protections
for volumetric attacks.

## Known gaps (tracked, post-v0.1.0)

- mTLS / authn for the operator snapshot gRPC stream (NetworkPolicy
  recommended meanwhile).
- Cosign signature verification for model artifacts (digest pinning today;
  ADR-0019 anticipates signatures).
- Webhook `failurePolicy: Fail` guidance for installations that want hard
  admission gating (default stays Ignore, ADR-0015).
- Credentialed (private) OCI registries for model artifacts.
