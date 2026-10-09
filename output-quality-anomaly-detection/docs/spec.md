# Output Quality & Anomaly Detection Policy

Category: Quality of Service
Applies to: llm, http, a2a, a2a_v1
Injection point: outbound
Version: 1.2.0

Inspects LLM and A2A-agent responses passing through Omni / Flex Gateway, as an **outbound** policy on the gateway-to-upstream leg, and scores each one for four quality risks — **hallucination**, **toxicity**, **bias** and **abnormal output** — using an LLM judge (any OpenAI-compatible chat completions API, e.g. OpenAI `gpt-5.4-mini`) that sees the request conversation and the response, with in-gateway word-list heuristics as the fallback when no judge is configured or it fails, always combined with structural signals the judge cannot observe — token confidence, finish reasons, repetition, a rolling per-replica length baseline and end-user feedback POSTed to a gateway-served endpoint. Each category has its own threshold and action (`monitor`, `annotate`, `block`), so the same policy can start as pure observability and be tightened category-by-category without touching the upstream model.

## Configuration

All properties are optional; the Rust defaults equal the `gcl.yaml` defaults.

### hallucinationThreshold
`number`, default `0.6`, range 0–1. Hallucination-risk score at or above which the response is flagged for `hallucination`.

### hallucinationAction
`string` enum `monitor | annotate | block`, default `monitor`. Action applied when `hallucination` is flagged. See Rule 6.

### toxicityThreshold
`number`, default `0.5`, range 0–1. Toxicity score at or above which `toxicity` is flagged.

### toxicityAction
`string` enum `monitor | annotate | block`, default `monitor`.

### biasThreshold
`number`, default `0.5`, range 0–1. Bias score at or above which `bias` is flagged.

### biasAction
`string` enum `monitor | annotate | block`, default `monitor`.

### anomalyThreshold
`number`, default `0.7`, range 0–1. Anomaly score at or above which `anomaly` is flagged.

### anomalyAction
`string` enum `monitor | annotate | block`, default `monitor`.

### toxicityTerms
`array<string>`, default `[]`. Heuristic path only (no judge, or judge failed). Extra case-insensitive words/phrases (matched on word boundaries) treated as toxic, weight `0.5` each, in addition to the built-in lexicon. Example: `["scumbag", "get lost"]`.

### biasGroupTerms
`array<string>`, default `[]`. Heuristic path only. Extra group nouns checked for sweeping generalisations (Rule 3), in addition to the built-in list. Use the plural form the model would write, e.g. `["engineers", "millennials"]`.

### lowConfidenceLogprob
`number`, default `-1.0`, `<= 0`. Mean token log-probability below which the response counts as low-confidence (Rule 4). Only effective when the client requested `logprobs: true`; otherwise this signal is skipped.

### maxInspectedBytes
`integer`, default `262144`, range 1 KiB – 10 MiB. Buffered JSON responses larger than this pass through unchanged and are logged as `uninspected`. For SSE, at most this many bytes of event data are accumulated; the rest of the stream is forwarded but not scored. Must be below the gateway's connection buffer limit for `annotate` / `block` to work on JSON.

### baselineWarmupSamples
`integer`, default `30`, `>= 1`. Responses a replica must observe before the length baseline contributes to the anomaly score.

### lengthZScoreThreshold
`number`, default `3.0`, `>= 1`. `|z|` of `ln(1 + chars)` against the baseline that contributes `0.8` to anomaly; `|z| >= 0.66 × threshold` contributes `0.4`.

### feedbackPath
`string`, default `/quality-feedback`. Exact path (query ignored) that the policy answers itself for feedback signals (Request flow). Empty string disables feedback entirely. The path is only reachable on APIs where this policy is applied, and it is subject to every policy that runs before this one (apply client-ID or JWT enforcement first to authenticate feedback).

### feedbackWindowSeconds
`integer`, default `300`, `>= 10`. Window length for the negative-feedback rate. The rate covers the current and previous window.

### feedbackMinSamples
`integer`, default `10`, `>= 1`. Minimum feedback signals across the two windows before Rule 5 can fire.

### feedbackNegativeRateThreshold
`number`, default `0.3`, range 0–1. Negative-feedback rate at or above which Rule 5 fires.

### judgeMode
`string` enum `off | onSuspicion | always`, default `always`. `always` calls the judge for every inspected non-empty response; `onSuspicion` only when the highest heuristic score is `>= judgeSuspicionScore` (cheaper, but misses what the word lists cannot see). The judge only runs when `judgeService` is set; if `judgeMode` is set explicitly without `judgeService`, the policy logs a warning at configure time and uses heuristics.

### judgeService
`string`, `format: service`, no default. Base URL of an OpenAI-compatible chat completions API — `https://api.openai.com`, Azure OpenAI, an Omni Gateway LLM proxy, vLLM or Ollama. Registered as an outbound cluster at gateway init. Empty → heuristics only, no outbound calls.

### judgePath
`string`, default `/v1/chat/completions`.

### judgeModel
`string`, default `gpt-5.4-mini`. Sent as `model`. Blank falls back to the default. Choose a small, fast model: the call is on the response path.

### judgeApiKey
`string`, `security:sensitive` (masked in API Manager), no default. Sent as `Authorization: Bearer <key>`. Never logged.

### judgeInstructions
`string`, no default. Operator rules appended to the rubric, e.g. `Never recommend competitor products.` Violations are scored under the closest category, or `anomaly`.

### judgeContextChars
`integer`, default `8000`, range 0–100000. Characters of the request conversation sent to the judge for grounding: the system prompt (up to a quarter of the budget) followed by the most recent turns. `0` sends only the response — hallucination is then judged against general knowledge.

### judgeJsonMode
`boolean`, default `true`. Sends `response_format: {"type":"json_object"}`. Disable for servers that reject it; the parser also tolerates prose or code fences around the JSON.

### judgeSuspicionScore
`number`, default `0.3`, range 0–1. Trigger for `onSuspicion` mode.

### judgeTimeoutMs
`integer`, default `5000`, range 100–30000. Judge call timeout; on expiry the heuristic scores are used.

### reportAll
`boolean`, default `false`. Attach the `x_output_quality` report to **every** inspected buffered response, not only flagged ones (`action: "monitor"`, `flagged: []` for clean answers). Intended for demos, tuning and client-side dashboards. SSE streams are never rewritten.

### stripAcceptEncoding
`boolean`, default `true`. Removes `Accept-Encoding` from the forwarded request so the upstream answers uncompressed. When `false`, compressed responses (`Content-Encoding` other than `identity`) pass through uninspected.

## Behavior

### Request flow
1. If feedback is enabled and the request path (without query) equals `feedbackPath`, the policy answers the request itself and **does not forward it upstream**:
   - Method other than `POST` → `405` `{"error":"method not allowed"}` with `Allow: POST`.
   - Body must be JSON `{"rating":"positive"|"negative","responseId"?:string,"category"?:"hallucination"|"toxicity"|"bias"|"anomaly"|"other","comment"?:string}`. Anything else → `400` `{"error":"<reason>"}`.
   - Valid → records the signal in the replica's feedback window and returns `202` `{"status":"recorded","windowSamples":n,"windowNegativeRate":r}`. Negative signals are logged at `warn` with `responseId` and `category` so feedback can be joined with the per-response evaluation log line.
   - If storage fails, still returns `202` with `"status":"accepted_unrecorded"` (feedback is best-effort telemetry).
2. If `x-output-quality-judge` equals this configuration's guard token, the request is the policy's own judge call (the judge API sits behind a gateway that also runs this policy): the header is removed and the exchange is forwarded **without evaluation**, preventing a judge-of-the-judge loop. The token is a hash of the policy configuration, so it is identical on every replica and unknown to clients; a mismatching header is removed and the request is evaluated normally.
3. If `stripAcceptEncoding` is `true`, removes `Accept-Encoding`.
4. For a JSON `POST` no larger than `maxInspectedBytes`, the request body is buffered (it is forwarded unchanged) and classified:
   - **A2A** — a JSON-RPC envelope whose `method` is `message/send`, `message/stream` (Legacy) or `SendMessage`, `SendStreamingMessage` (v1), or a v1 HTTP+JSON call to `…/message:send` / `…/message:stream`. The user's text parts (`params.message.parts[].text`, or `message.parts` for HTTP+JSON) become the judge context `USER: <text>`.
   - **Other A2A methods** (`GetTask`, `tasks/get`, `CancelTask`, push-config, any other JSON-RPC method) — the response is **not evaluated**, so a task fetched again is not re-scored and re-billed.
   - **Anything else** — treated as an LLM API call; the conversation transcript (Chat Completions `messages`, Responses `instructions`/`input`, Anthropic `system`/`messages`, legacy `prompt`) is kept for the judge when `judgeContextChars > 0`.

### Response flow
Only responses with a 2xx status are inspected. Non-2xx, bodyless, compressed (when not stripped) and non-JSON/non-SSE responses pass through untouched.

- **JSON** (`Content-Type` contains `json`): the body is buffered (bounded by `maxInspectedBytes`), text is extracted, scored and an action applied. If any category with action `annotate` or `block` exists in config, `Content-Length` is removed in the headers phase so the body can be rewritten.
- **SSE** (`Content-Type` contains `text/event-stream`): the stream is read chunk-by-chunk and forwarded unmodified while text deltas, `finish_reason`s and logprobs are accumulated. Scoring happens when the stream ends. `annotate` and `block` cannot be applied to a stream that has already been delivered; they are downgraded to `monitor` and the log line records `action=monitor(streaming)`.

Recognised response shapes (non-stream and stream):

- **A2A agents** (checked first) — the JSON-RPC `result` (or the bare v1 HTTP+JSON body), unwrapped from `task` / `message` / `statusUpdate` / `artifactUpdate` when present. Text is taken from `artifacts[].parts[].text`, a streaming `artifact.parts[].text`, Message `parts[].text`, and `status.message.parts[].text` **only when the task has no artifact text** (artifacts are the output; a completed task's status note such as "Answer ready." is not scored). Parts are text when `kind`/`type` is `text` or absent (v1 bare `{ "text": … }`); file/data parts are ignored. `status.state` maps to a finish reason: `TASK_STATE_COMPLETED`/`completed`/input-required → `stop`, `TASK_STATE_FAILED`/`failed`/`rejected` → `failed`. JSON-RPC `error` responses and agent cards are not scored.
- OpenAI Chat Completions (`choices[].message.content` string or text parts, `choices[].delta.content`, `logprobs.content[].logprob`), OpenAI legacy Completions (`choices[].text`, `logprobs.token_logprobs`), OpenAI Responses API (`output[].content[].text`, `response.output_text.delta`), and Anthropic Messages (`content[].text`, `content_block_delta`, `stop_reason`). Tool calls count as non-empty output. Unrecognised JSON is not scored.

### Injection point
The definition declares `metadata/capabilities/injectionPoint: outbound`, so in API Manager the policy is listed under **Outbound policies** and is bound to an upstream of the instance. It runs after inbound policies (authentication, rate limiting) and sees exactly what the upstream agent or model returned. The feedback endpoint is still answered by the policy at this hook — verified on managed Omni Gateway 1.13.1 — so `POST <base-path>/quality-feedback` never reaches the upstream.

### Rules
Signals fall into two groups. **Content** signals judge meaning — the LLM verdict (Rule 2) or, when it is unavailable, the word-list heuristics (Rules 1, 3 and the phrase part of 4). **Structural** signals are things the judge cannot observe — token logprobs (Rule 4), finish reasons, empty/looping/garbled output, length baseline and feedback (Rule 5) — and are always combined in. A category's score is the noisy-OR of its signals, `1 − Π(1 − wᵢ)`. Heuristic text matching is case-insensitive on word boundaries.

1. **Toxicity** — built-in threat/harassment phrases (`0.6` each, e.g. "kill yourself"), insults (`0.35`, e.g. "idiot"), profanity (`0.25`), `toxicityTerms` (`0.5`), and shouting (`0.2`: ≥ 40 letters, > 70 % upper-case).
2. **LLM judge** — the judge receives a fixed rubric (system message, plus `judgeInstructions`) and a user message `<context>…</context><response>…</response>` holding the transcript and up to 24 KiB of response text. Both blocks are declared untrusted and any `<context>`/`<response>` tags inside them are escaped, so evaluated text cannot close its block and inject scoring instructions. No `temperature` is sent (GPT-5-family models reject non-default values). The reply must contain a JSON object `{"hallucination":n,"toxicity":n,"bias":n,"anomaly":n,"reasons":{…}}`; the first `{` to last `}` is parsed, values are clamped to 0–1 and numeric strings accepted. When readable, these four scores **replace** the heuristic content scores (so a quoted or refuted slur that the word lists would flag is cleared), and reasons appear as `<category>:llm(<reason>)` — only for scores ≥ 0.3 or when the judge gave one. The judge's `anomaly` covers off-topic, non-answers, incoherence, wrong language and leaked system prompts.
3. **Bias** — a group noun (built-in list or `biasGroupTerms`) followed within three words by a copula/modal (`are`, `is`, `can't`, `should`, `tend`, `always`, `never`, …) and, within six words, a generalising marker. Each distinct (group, marker) pair is one signal: dehumanising/derogatory markers (`inferior`, `criminals`, `subhuman`, …) weigh `0.55`; generalising markers (`always`, `naturally`, `bad at`, …) weigh `0.35` — so "Women are naturally bad at math" scores `1 − 0.65² ≈ 0.58`. A preceding `all`/`every` adds a hit of `0.35` on its own when a copula follows.
4. **Hallucination risk** — mean token logprob below `lowConfidenceLogprob` (`0.5`, or `0.7` below twice the threshold); unsupported attributions such as "studies show", "experts agree" (`0.2` each); DOI citations (`0.2`); numeric `[n]` reference markers with no URL in the text (`0.2`); "et al." citations (`0.15`); knowledge-cutoff / no-real-time-access disclaimers (`0.2`); self-corrections such as "I apologize for the confusion" (`0.15`); hedging density of ≥ 3 hedges and ≥ 2 per 100 words (`0.25`).
5. **Anomaly** — empty output without tool calls (`0.9`); `finish_reason` `length`/`max_tokens` (`0.45`) or `content_filter` (`0.8`); repeated word 4-grams ≥ 30 % over ≥ 30 words (`0.5`–`0.9`); ≥ 5 % replacement/control characters (`0.7`); length z-score (see `lengthZScoreThreshold`); an A2A task that ended `failed`/`rejected` (`0.6`); and a **feedback anomaly** — negative-feedback rate ≥ `feedbackNegativeRateThreshold` with ≥ `feedbackMinSamples` samples (`0.75`, so it alone flags at the default threshold). Every inspected response updates the baseline after its own score is computed.
6. **Action precedence** — if any flagged category's action is `block`, the response is blocked; else if any is `annotate`, it is annotated; else it is monitored. Every flagged response generates an Anypoint policy violation and a `warn` log line (`flagged response_id=… categories=… action=… source=… judge_ms=… scores=[…] reasons=[…]`) regardless of action; unflagged responses log one `info` line (`evaluated response_id=… source=… judge_ms=… scores=[…]`). Response text and `judgeApiKey` are never logged.
   - `annotate` adds an `x_output_quality` object (`flagged`, `action`, `source`, `judgeMs`, `scores`, `reasons`). For LLM APIs it is a top-level field of the JSON body; for **A2A** it goes into the Task / Message `metadata` (`result.task.metadata.x_output_quality`, or `result.metadata…`), which is A2A-native and keeps the JSON-RPC envelope free of extra members. `source` is `llm`, `heuristic` (no judge configured / `onSuspicion` skipped it) or `heuristic_fallback` (judge failed). `judgeMs` is the judge round-trip, `null` when not called.
   - `block` on a Chat Completions body replaces every `choices[].message.content` with a withheld notice, sets `finish_reason` to `content_filter` (the convention OpenAI clients already handle) and adds `x_output_quality`; for **A2A** every text part (artifacts, status message, message parts) is replaced by the withheld notice while the task structure, ids, state and non-text parts stay intact, and the report goes into `metadata`; other shapes are replaced by `{"error":{"type":"output_quality_violation","code":"<category>",...},"x_output_quality":{...}}`. The upstream status code is preserved because response headers are committed before the body is read.

### Missing context (local-mode)
The policy reads no control-plane context (`environment`, `api`, `tiers`, `identityManagement`), so it behaves identically in local mode and logs nothing about missing context. The judge is the only outbound dependency and is configured statically.

### Failure modes
The policy **fails open** everywhere — it is a quality monitor, not a security control:
- Judge timeout, transport error, non-2xx (e.g. `401` bad key, `429` rate limit) or a reply with no readable verdict → the word-list heuristics score the content (`source: heuristic_fallback`), `warn` logs with the judge status and `judge_unavailable`. The response is never held back because the judge failed.
- Local storage error or repeated CAS conflict (3 attempts) → that response skips the baseline/feedback signals, `warn` log.
- Body rewrite failure (`set_body` error) → original body is delivered, `error` log; the violation is still reported.
- Unparsable JSON / SSE events → not scored, `debug` log.

### Edge cases
- State is **per replica** (`LocalDataStorage`, shared by a replica's Envoy workers). With N replicas behind a load balancer each replica estimates the same rates from its share of traffic; baselines warm up per replica. Remote shared storage is deliberately not used because `DataStorageBuilder::remote` panics when the gateway has no shared storage configured.
- Multiple choices (`n > 1`), multiple artifacts and multiple text parts are concatenated and scored as one output.
- `judgeService` must actually be saved in the policy configuration. If API Manager saves the policy without it, every report shows `source: "heuristic"` and `judgeMs: null` — the word lists alone are scoring. Check the saved configuration before blaming the judge.
- Empty responses are not sent to the judge (structural `empty_output` already flags them).
- The judge sees the request conversation and response text: point `judgeService` only at a provider your data-handling rules allow. Set `judgeContextChars: 0` to send the response alone.
- LLM verdicts are probabilistic: the same response can score slightly differently across calls.
- `block` cannot change a 200 to an error status (see Rule 6); clients must treat `finish_reason: content_filter` / `x_output_quality.action == "block"` as the signal.
- Feedback requests count toward any rate limits applied before this policy.

### Error responses
| Condition | Status | Body |
|---|---|---|
| Feedback with non-POST method | 405 | `{"error":"method not allowed"}` |
| Feedback body not JSON / missing or invalid `rating` / invalid `category` | 400 | `{"error":"<reason>"}` |
| Feedback accepted | 202 | `{"status":"recorded",...}` |

No error status is ever produced for proxied LLM traffic.

### Benchmarks
Latency budget: heuristics on a 4 KB completion < 1 ms per response. With `judgeMode: always` every response also waits for one judge call (typically 1–3 s for a small hosted model, capped by `judgeTimeoutMs`); use `onSuspicion` where latency matters more than recall. No Criterion bench yet — add one under `benches/` covering a clean 4 KB completion, a flagged completion, and a 256 KB completion before raising `maxInspectedBytes` defaults.

## Architecture

- `lib.rs` — entrypoint and async orchestrators (request filter, feedback handler, JSON and SSE response paths, storage updates, judge call).
- `settings.rs` — resolves `generated::config::Config` into validated `Settings` with defaults.
- `extract.rs` — response text/logprob/finish-reason extraction for LLM JSON bodies, A2A Task/Message payloads and SSE events; A2A request classification (`a2a_request`); request conversation transcripts for the judge.
- `sse.rs` — incremental `text/event-stream` event splitter.
- `detect.rs` — pure heuristic rules producing an `Analysis` of content scores (replaceable by the judge) and structural scores (always applied).
- `stats.rs` — rolling length baseline and two-bucket feedback window.
- `judge.rs` — LLM rubric, injection-safe request builder and verdict parser (Rule 2).
- `verdict.rs` — thresholds, action precedence, annotate / block body rewriting (Rule 6).
- `feedback.rs` — feedback body parsing.

Everything except `lib.rs` is synchronous and unit-tested directly; `lib.rs` is exercised end-to-end with `pdk-unit`.

## Examples

OpenAI judge on every response, block toxicity, annotate bias (in API Manager the same fields appear in the policy form; `judgeApiKey` is masked):

```yaml
policies:
  - policyRef:
      name: output-quality-anomaly-detection-flex-v1-0
    config:
      judgeService: https://api.openai.com
      judgeModel: gpt-5.4-mini
      judgeApiKey: <OpenAI API key>
      judgeInstructions: Never give medical dosage advice.
      toxicityAction: block
      biasAction: annotate
```

Flagged non-streaming response with `biasAction: annotate`:

```json
{
  "id": "chatcmpl-1",
  "object": "chat.completion",
  "choices": [{"index": 0, "message": {"role": "assistant", "content": "Women are naturally bad at math."}, "finish_reason": "stop"}],
  "x_output_quality": {
    "flagged": ["bias"],
    "action": "annotate",
    "source": "llm",
    "scores": {"hallucination": 0.0, "toxicity": 0.0, "bias": 0.9, "anomaly": 0.0},
    "reasons": ["bias:llm(Generalises a trait to a whole gender.)"]
  }
}
```

Feedback signal:

```http
POST /quality-feedback HTTP/1.1
Content-Type: application/json

{"responseId": "chatcmpl-1", "rating": "negative", "category": "hallucination"}
```

```json
{"status": "recorded", "windowSamples": 14, "windowNegativeRate": 0.36}
```

A2A `SendMessage` reply through the gateway, blocked for toxicity (policy `reportAll: true`):

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "result": {
    "task": {
      "id": "c255ce63-…",
      "contextId": "9edf581f-…",
      "status": { "state": "TASK_STATE_COMPLETED" },
      "artifacts": [{ "artifactId": "answer-c255ce63-…", "name": "answer",
                      "parts": [{ "text": "This response was withheld because it did not meet output quality requirements." }] }],
      "metadata": {
        "x_output_quality": {
          "flagged": ["toxicity", "anomaly"],
          "action": "block",
          "source": "llm",
          "judgeMs": 1872,
          "scores": { "hallucination": 0.0, "toxicity": 1.0, "bias": 0.0, "anomaly": 0.7 },
          "reasons": ["toxicity:llm(It directly insults and demeans the user…)", "anomaly:llm(It does not answer the user's request…)"]
        }
      }
    }
  }
}
```

## Compatibility

- Built with PDK 1.10 (`cargo-anypoint` 1.10.0; 1.6.x cannot generate the GCL). Verified on local Flex Gateway 1.13.0 (playground + `pdk-test`) and managed Omni Gateway 1.13.1 in CloudHub 2.0.
- A2A: Legacy (`message/send`, `kind`-tagged parts) and v1.0 (`SendMessage`, bare parts, `TASK_STATE_*`), as produced by the MuleSoft A2A Connector 2.0.0. The Exchange asset-type token for A2A v1 is `a2a_v1` (Exchange rejects `a2av1`).
- Judge verified with OpenAI `gpt-5.4` and `gpt-5.4-mini`.

