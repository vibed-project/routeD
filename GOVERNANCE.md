# Governance

routeD is an open-source project licensed under Apache-2.0. This document
describes how decisions are made while the project is pre-1.0.

## Roles

- **Maintainers** have write access, review and merge changes, cut releases,
  and are responsible for the security process. The initial maintainer team is
  the `vibed-project` organisation owners.
- **Contributors** are anyone who submits changes, issues, or documentation.
  All contributions require a Developer Certificate of Origin sign-off (see
  `DCO` and `CONTRIBUTING.md`).

## Decision making

- Day-to-day decisions are made by lazy consensus on pull requests.
- Decisions that establish or alter a contract another component depends on
  (CRD schemas, the Decision JSON, header semantics, snapshot format, extension
  seams) require an Architecture Decision Record in `docs/adr/`.
- Disagreements are resolved by maintainer majority.

## Adding maintainers

A contributor with a sustained record of high-quality contributions may be
nominated by an existing maintainer and confirmed by maintainer majority.

## Extension seams

Everything in this repository is and remains Apache-2.0. Extension points
for third-party integrations are documented traits in core
(`docs/adr/0004-extension-seams.md`). A seam never disables a core feature,
and core ships a working default for every seam.
