# P4A submission — copy/paste content for the wizard

Start at https://www.p4a.ai/dashboard/policies → **Submit Policy**.

## 1. Basics

**Name:** `Output Quality & Anomaly Detection`

## 2. Repository

| Field | Value |
|---|---|
| Project type | **Unified-model** (single crate, `.project.yaml` at the repository root) |
| Repository URL | `https://github.com/chiewklee/output-quality-anomaly-detection-policy` |
| Project root | repository root (the policy is at the top level) |
| Branch / tag / commit | `main` — or pin the commit SHA shown on GitHub for this submission |

Expected validation banner: **Valid policy (PDK 1.10.0)**.

## 3. Classification

| Field | Value |
|---|---|
| Asset kinds | **agent**, **llm** (and **api** — the policy also accepts `http` instances) |
| A2A versions | **v0.3.0** and **v1.0** |
| Direction | **outbound** (inferred from `metadata/capabilities/injectionPoint: outbound`) |

## 4. Description (Overview tab)

```markdown
**Output Quality & Anomaly Detection** is an outbound Omni / Flex Gateway policy that inspects what your AI agents and LLMs return — and catches answers that are **hallucinated, biased, toxic or abnormal** before they reach users.

### Why
Agents increasingly answer users directly. Guardrails on the *prompt* don't tell you whether the *answer* was any good. This policy judges every response on the gateway, with no change to the agent or model.

### How it decides
- **LLM judge** — any OpenAI-compatible model (e.g. OpenAI `gpt-5.4`) scores each answer 0–1 for hallucination, toxicity, bias and anomaly, seeing the user's question for grounding. Prompt-injection-safe: the evaluated text cannot rewrite its own verdict.
- **Word-list fallback** — if no judge is configured or it fails, built-in heuristics score the same categories. The policy fails open and never holds a response because the judge is down.
- **Structural signals** — always on: low token confidence, empty / truncated / looping / garbled output, failed A2A tasks, response-length outliers.
- **User feedback** — a gateway-served `POST /quality-feedback` endpoint; a spike in negative ratings raises an anomaly across all traffic.

### What it does with a verdict
Per category: **monitor** (gateway log + quality report), **annotate** (deliver with a quality report attached) or **block** (withhold the text). Start with everything on monitor, then tighten category by category.

### Works with
- **A2A agents** — Legacy (`message/send`) and v1.0 (`SendMessage`), JSON-RPC and HTTP+JSON, including streaming. The report goes into the task's `metadata.x_output_quality`; blocked tasks keep their ids and state.
- **LLM APIs** — OpenAI Chat Completions, Completions and Responses API, Anthropic Messages (JSON and SSE).

Verified on managed Omni Gateway 1.13.1 (CloudHub 2.0) with a MuleSoft A2A Connector 2.0 agent, and on Flex Gateway 1.13.0.
```

## 5. Documentation tabs

### Configuration

```markdown
All fields are optional; defaults shown.

| Field | Default | Description |
|---|---|---|
| `judgeService` | — | Base URL of an OpenAI-compatible chat completions API, e.g. `https://api.openai.com`. Empty → word-list heuristics only (no outbound calls). |
| `judgeModel` | `gpt-5.4-mini` | Judge model. Use a different model from the one your agent uses. |
| `judgeApiKey` | — | Bearer key for the judge (secret, never logged). |
| `judgeMode` | `always` | `always`, `onSuspicion` (only when heuristics see something) or `off`. |
| `judgeInstructions` | — | Extra operator rules, e.g. "Never give medical dosage advice." |
| `judgeContextChars` | `8000` | Characters of the user conversation sent to the judge for grounding (0 = answer only). |
| `judgeTimeoutMs` | `5000` | On timeout the heuristics are used. |
| `hallucinationAction` / `toxicityAction` / `biasAction` / `anomalyAction` | `monitor` | `monitor`, `annotate` or `block`. |
| `hallucinationThreshold` / `toxicityThreshold` / `biasThreshold` / `anomalyThreshold` | `0.6` / `0.5` / `0.5` / `0.7` | Score (0–1) at which a category is flagged. |
| `reportAll` | `false` | Attach the report to every inspected response, not only flagged ones. |
| `feedbackPath` | `/quality-feedback` | Gateway-served feedback endpoint (empty disables). |
| `feedbackWindowSeconds` / `feedbackMinSamples` / `feedbackNegativeRateThreshold` | `300` / `10` / `0.3` | Negative-feedback anomaly: rate ≥ threshold over ≥ N samples in the window. |
| `toxicityTerms` / `biasGroupTerms` | `[]` | Extra terms for the heuristic fallback. |
| `lowConfidenceLogprob`, `maxInspectedBytes`, `baselineWarmupSamples`, `lengthZScoreThreshold`, `judgePath`, `judgeJsonMode`, `judgeSuspicionScore`, `stripAcceptEncoding` | see spec | Advanced tuning. |

Full reference: https://github.com/chiewklee/output-quality-anomaly-detection-policy/blob/main/docs/spec.md
```

### Examples

````markdown
**Apply as an outbound policy** on an A2A agent instance (API Manager → instance → Outbound policies):

```yaml
judgeService: https://api.openai.com
judgeModel: gpt-5.4
judgeApiKey: <OpenAI key>
toxicityAction: block
biasAction: annotate
hallucinationAction: annotate
anomalyAction: annotate
```

**A2A reply flagged for hallucination** (`SendMessage` → `result.task`):

```json
{
  "artifacts": [{ "parts": [{ "text": "The telephone was invented by Thomas Edison in 1921 at MIT…" }] }],
  "metadata": {
    "x_output_quality": {
      "flagged": ["hallucination"],
      "action": "annotate",
      "source": "llm",
      "judgeMs": 1629,
      "scores": { "hallucination": 1.0, "toxicity": 0.0, "bias": 0.0, "anomaly": 0.2 },
      "reasons": ["hallucination:llm(It falsely attributes the telephone to Thomas Edison with incorrect date, place, and fabricated study/DOI details.)"]
    }
  }
}
```

**Blocked toxic reply** — every text part becomes "This response was withheld because it did not meet output quality requirements.", with the same report (`action: "block"`).

**User feedback:**

```http
POST /quality-feedback
Content-Type: application/json

{"rating": "negative", "responseId": "task-123", "category": "hallucination"}
```
→ `202 {"status":"recorded","windowSamples":3,"windowNegativeRate":0.67}`

A runnable A2A demo agent with scripted misbehaviour modes and a UI is in the repository: `output-quality-demo-agent/` (see `DEMO.md`).
````

### FAQ

```markdown
**Does it change the HTTP status when it blocks?** No — response headers are already sent when the body is judged. Blocked A2A tasks keep their state; the text is withheld and `metadata.x_output_quality.action` is `block`. For Chat Completions, `finish_reason` becomes `content_filter`.

**What happens if the judge is down or slow?** The word-list heuristics score the answer (`source: heuristic_fallback`) and the response is delivered — the policy fails open.

**How much latency does it add?** Heuristics < 1 ms. With the judge on every response, one judge call per answer (≈1–2.5 s with OpenAI `gpt-5.4`). Use `judgeMode: onSuspicion` to only call the judge when heuristics see something.

**Streaming responses?** Inspected and reported, but cannot be annotated or blocked after they are delivered (logged as `monitor(streaming)`).

**Does the judge see user data?** Yes — the user's message (bounded by `judgeContextChars`) and the answer. Point `judgeService` only at a provider your data rules allow, or set `judgeContextChars: 0`.

**Why "outbound"?** It runs on the gateway-to-upstream leg, after inbound policies such as authentication, and sees exactly what the agent returned.

**Is state shared across gateway replicas?** No — the feedback window and length baseline are per replica (in-memory), which keeps the policy safe on gateways without shared storage.
```

## 6. Links & media

| Field | Value |
|---|---|
| Examples link | `https://github.com/chiewklee/output-quality-anomaly-detection-policy/tree/main/output-quality-demo-agent` |
| Icon | `icon.svg` at the repository root (picked up automatically) |
| Video | — |

## 7. Review → **Submit for Review**
