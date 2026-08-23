# ADR-0015: Admission validation

## Status

Accepted

## Context

ADR-0014 deferred the validating webhook from the phase 3 scope so the
reconciler, distribution and leader election could ship first. This ADR
records the webhook's contract now that it lands. Until an object is
admitted, the only feedback for a broken CRD is asynchronous
(`status.conditions[Ready]` after the next reconcile, or `routedctl
validate` run by hand); admission-time rejection puts the same diagnostic in
the `kubectl apply` error instead.

## Decision

### Same compiler, attributed diagnostics

The webhook (`POST /validate` on the operator) validates by running
`routed_policy::compile` - the identical code path as the reconciler and
`routedctl validate` (ADR-0008) - over current cluster state with the
incoming object substituted (replaced by `namespace/name`, or appended). A
write is denied only by **error diagnostics attributed to the incoming
object** (matching `kind` + `namespace/name`); errors on other objects never
block an unrelated write, and warnings attributed to the object are returned
as admission warnings. `CREATE` and `UPDATE` of the four routed.io kinds are
validated; `DELETE` is allowed unexamined (a deletion that breaks
cross-references surfaces on the referencing objects' conditions).

### Fail open by default

`failurePolicy: Ignore` is the chart default, and the webhook itself allows
the write when it cannot read cluster state. Rationale: the operator's
status conditions remain the authoritative async safety net, and a webhook
outage must not lock anyone out of editing the very CRDs that might fix it.
Installations that want hard gating set
`operator.webhook.failurePolicy=Fail` (the e2e does, to make denial tests
deterministic).

### Helm-generated certificate, reused across upgrades

The chart generates a self-signed certificate (`genSelfSignedCert`, ten
years, SANs for the operator service DNS names) on first install and stores
it in a `kubernetes.io/tls` Secret; on upgrade a `lookup` reuses the stored
certificate so the `ValidatingWebhookConfiguration.caBundle` and the served
certificate can never disagree. The operator loads `tls.crt`/`tls.key` from
the mounted Secret at startup; the webhook server is enabled by
`--webhook-certs-dir` and serves HTTP/1.1 only (the API server negotiates
via ALPN).

### No mutation

The webhook never patches objects. Defaulting stays in the CRD schemas and
the compiler; a validating-only webhook keeps `sideEffects: None` honest and
the trust story simple.

## Consequences

- A bad CRD is rejected with the exact compiler diagnostic in the `kubectl`
  error message, and the same text appears in `status.conditions` if it gets
  in some other way.
- Because validation lists cluster state per admission, a burst of CRD
  writes costs one list set each; acceptable at configuration-change rates.
- Certificate rotation is manual: delete the Secret and upgrade (or replace
  it with cert-manager-managed material; the operator only reads files).

## Alternatives considered

- cert-manager for the webhook certificate: rejected as a hard dependency;
  installations that run it can point the Secret at a cert-manager
  `Certificate` instead.
- Validating in isolation (compile only the incoming object): rejected;
  cross-references (tiers, data classes, profiles) are the majority of real
  mistakes and need cluster state.
- `failurePolicy: Fail` by default: rejected; it turns any webhook outage
  into a CRD write outage, and the deferral reasoning in ADR-0014 was
  precisely about not shipping a lockout risk casually.
