# Examples

Each `NNN-<outcome>-<scenario>/` directory is a self-contained request + policy
combination that doubles as documentation and as a golden test
(`cmd/routedctl/tests/golden.rs`). Files:

| File | Purpose |
|------|---------|
| `resources.yaml` | ModelTiers, DataClasses, RoutingPolicies, RouterProfile |
| `request.json` | OpenAI / Anthropic format request body |
| `headers.json` | Optional request headers (`X-Routed-*`) |
| `findings.json` | Optional classifier findings (bypasses the heuristic classifier) |
| `overrides.json` | Optional engine-input overrides (token estimates) |
| `path.txt` | Optional request path (default `/v1/chat/completions`) |
| `expected.decision.json` | Golden decision; regenerate with `UPDATE_GOLDEN=1 make test` |

Try one:

```sh
routedctl explain --dir examples/004-route-personal-header-eu-only
```
