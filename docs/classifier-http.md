# External classifier contract (`type: http`)

A `RouterProfile` can delegate classification to an HTTP service:

```yaml
spec:
  classifier:
    type: http
    uri: http://classifier.ai-platform.svc:8000/classify
    timeoutMs: 25
```

routeD sends `POST <uri>` with `Content-Type: application/json` (and
`Authorization: Bearer $ROUTED_CLASSIFIER_TOKEN` when that variable is set):

```json
{
  "systemPrompt": "truncated system prompt or null",
  "userText": "last user message",
  "history": ["earlier user turns"],
  "toolOutputs": ["tool results"]
}
```

and expects `200 OK` with a `Findings` document:

```json
{
  "task": "summarization",
  "complexity": "medium",
  "riskScore": 0.12,
  "piiEntities": ["EMAIL"],
  "piiConfidence": { "EMAIL": 0.95 },
  "inferredDataClass": "personal"
}
```

All fields are optional except that `riskScore` should always be present (an
absent score is treated as degraded classification and triggers the policy
fallback). Any non-200 status, malformed body, or response slower than
`timeoutMs` is a degraded classification: routeD never guesses.
