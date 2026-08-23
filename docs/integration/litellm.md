# LiteLLM

Two integration shapes work with a LiteLLM proxy; pick one.

## Option A: inline mode in front of LiteLLM

Point clients at routeD and routeD at LiteLLM. No LiteLLM configuration
needed:

```sh
routed serve --mode inline --upstream http://litellm:4000 \
  --resources /etc/routed/resources
```

LiteLLM receives the rewritten `model` and enforces it exactly as if the
client had asked for it. Keep `failure` behaviour fail-closed by fronting
routeD with your ingress; routeD refuses readiness without a snapshot.

## Option B: decision API from a pre-call hook

Keep clients pointed at LiteLLM and call `POST /v1/decide` from a
[custom callback](https://docs.litellm.ai/docs/proxy/call_hooks). The hook
sends the raw request body plus the caller's headers and applies the
returned decision:

```python
# callbacks.py: attach with `callbacks: custom_callbacks.proxy_handler_instance`
import httpx
from litellm.integrations.custom_logger import CustomLogger

ROUTED = "http://routed:8080"

class RoutedHook(CustomLogger):
    async def async_pre_call_hook(self, user_api_key_dict, cache, data, call_type):
        headers = {"X-Routed-Path": "/v1/chat/completions"}
        # Forward caller identity / data-class hints if you have them:
        # headers["X-Routed-Tenant"] = ...
        async with httpx.AsyncClient(timeout=2.0) as client:
            r = await client.post(f"{ROUTED}/v1/decide", json=data, headers=headers)
        r.raise_for_status()
        decision = r.json()
        if decision["outcome"] == "BLOCK":
            raise ValueError(f"blocked by routing policy: {decision.get('reason')}")
        if decision["outcome"] == "ROUTE":
            data["model"] = decision["gatewayModel"]
            for k, v in decision.get("parameters", {}).items():
                data.setdefault(k, v)
        return data

proxy_handler_instance = RoutedHook()
```

Fail closed: let the hook raise when routeD is unreachable rather than
falling through to the requested model.

Send outcomes back for the phase 6 feedback loop with
`POST /v1/feedback` (`decisionId` from the `X-Routed-Decision-Id` header or
the decision JSON).
