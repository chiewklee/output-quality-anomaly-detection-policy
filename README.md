# Output Quality & Anomaly Detection for agents (P4A)

An Omni / Flex Gateway policy that detects **hallucination, bias, toxicity and abnormal output** in what LLMs and A2A agents return — using an LLM judge, word-list fallback, structural signals and user feedback — and monitors, annotates or blocks each category. Plus a demo A2A agent and UI to show it live on CloudHub 2.0.

| Folder | What it is | Start here |
|---|---|---|
| [`output-quality-anomaly-detection/`](output-quality-anomaly-detection/) | The PDK 1.10 policy (Rust → WASM), outbound, v1.2.0 | [README](output-quality-anomaly-detection/README.md) · [spec](output-quality-anomaly-detection/docs/spec.md) |
| [`output-quality-demo-agent/`](output-quality-demo-agent/) | Mule 4.12 A2A 1.0 demo agent + UI | [README](output-quality-demo-agent/README.md) · [demo runbook](output-quality-demo-agent/DEMO.md) |

```
caller ──► agent-network-ingress-gw (Omni Gateway, clee-inc-ps)
             └─ outbound: Output Quality & Anomaly Detection ──► OpenAI gpt-5.4 (judge)
                   └─► output-quality-demo-agent (CloudHub 2.0) ──► OpenAI gpt-5.4-mini (answers)
```

**Live demo:** `https://agent-network-ingress-gw-ky9yvn.aqopru.usa-e1.cloudhub.io/quality-demo/` (add `?autorun=bias`, `toxic`, `quoted`, `hallucinate`, `offtopic`, `leak`, `loop`, `normal`).

## How it decides

1. **LLM judge** (any OpenAI-compatible API) scores the agent's answer 0–1 per category, seeing the user's question for grounding. Prompt-injection-safe delimiters; its verdict replaces the word lists.
2. **Word lists** score the same categories when no judge is configured or it fails (`source: heuristic_fallback`) — the policy never goes blind and never holds a response because the judge is down.
3. **Structural signals** always add in: low token confidence, empty / truncated / looping / garbled output, failed A2A tasks, length outliers, and a spike in negative user feedback.
4. Per category: score ≥ threshold → flagged → `monitor` (log + violation), `annotate` (report attached) or `block` (text withheld). Strictest wins.

## Status (2026-10-09)

| Item | State |
|---|---|
| Policy in Exchange | `output-quality-anomaly-detection` 1.0.0 → 1.1.0 → **1.2.0** (outbound) |
| Tests | 102 unit + pdk-unit tests, Docker integration test, local playground, live CloudHub 2.0 run |
| Demo agent | Exchange `output-quality-demo-agent` 1.1.0 (type agent) + `output-quality-demo-agent-app` 1.1.0 (app); CloudHub 2.0 app running, internal endpoint only |
| Gateway instance | `21226057` on `agent-network-ingress-gw`, outbound policy applied with OpenAI judge |
| P4A submission | repo public; wizard content ready in [`P4A_SUBMISSION.md`](P4A_SUBMISSION.md) |

## Lessons worth keeping

- Exchange asset type for A2A v1 is `a2a_v1` (the PDK skill's `a2av1` is rejected).
- `agent`-type Exchange assets need the `agent-metadata` zip and `protocol`/`platform` passed as properties; an `agent-card.json` upload becomes a plain A2A asset.
- API Manager saved the policy without `judgeService` the first time — check the saved config if reports say `source: heuristic`.
- A2A Connector 2.0: the completing status update needs a message with at least one part.
- Use different models for the agent and the judge (here `gpt-5.4-mini` vs `gpt-5.4`).
