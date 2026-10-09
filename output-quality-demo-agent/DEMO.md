# Output Quality & Anomaly Detection — demo runbook

```
Browser (demo UI, same origin)
   │  https://agent-network-ingress-gw-ky9yvn.aqopru.usa-e1.cloudhub.io/quality-demo/
   ▼
Omni Gateway  agent-network-ingress-gw   (managed, Flex 1.13.1, private space clee-inc-ps, Sandbox)
   │  agent instance 21226057  (asset output-quality-demo-agent 1.1.0, base path /quality-demo)
   │  OUTBOUND policy: Output Quality & Anomaly Detection 1.2.0
   │     judge: OpenAI gpt-5.4  ── calls https://api.openai.com
   ▼
output-quality-demo-agent   (Mule 4.12.3 Edge, Java 17, CloudHub 2.0, internal endpoint only)
   ├─ A2A 1.0 server: /quality-demo-agent/jsonrpc  (+ /.well-known/agent-card.json)
   ├─ Normal mode     → OpenAI gpt-5.4-mini (real answer)
   └─ scripted modes  → canned bad answers (bias, toxic, hallucinate, quoted, offtopic, leak, loop, empty)
```

**Say this in the demo:** the bad answers are *scripted* (real models rarely misbehave on cue — `gpt-5.4-mini` refused every provocation we tried); the policy and its LLM judge are *real*, and the judge is a different, larger model than the agent.

## Deployed resources (Sandbox)

| Resource | Value |
|---|---|
| Public demo URL | `https://agent-network-ingress-gw-ky9yvn.aqopru.usa-e1.cloudhub.io/quality-demo/` |
| A2A endpoint (JSON-RPC) | `…/quality-demo/quality-demo-agent/jsonrpc` (header `A2A-Version: 1.0`) |
| Agent card | `…/quality-demo/quality-demo-agent/.well-known/agent-card.json` |
| Gateway | `agent-network-ingress-gw` (`b5dcfb28-ef87-4fee-8375-0de2077a5782`) |
| Agent instance | `21226057`, upstream `https://output-quality-demo-agent-ky9yvn.internal-aqopru.usa-e1.cloudhub.io/` |
| Outbound policy | `output-quality-anomaly-detection` 1.2.0 (policy id `9480349`) |
| CloudHub 2.0 app | `output-quality-demo-agent` (`48b0a0d0-6b65-4833-b3f0-394fd345589c`), 1 × 0.1 vCore, private space `clee-inc-ps` (`929756ed-619c-4974-9f92-ffb172c08b4f`) |
| App properties | `openai.model=gpt-5.4-mini`, `agent.publicUrl=https://agent-network-ingress-gw-ky9yvn.aqopru.usa-e1.cloudhub.io/quality-demo`, protected `openai.apiKey` |
| Exchange: agent | `output-quality-demo-agent` 1.1.0 — type **agent**, `protocol: a2a`, `platform: mulesoft` |
| Exchange: Mule app | `output-quality-demo-agent-app` 1.1.0 — type app (the deployable jar) |
| Exchange: policy | `output-quality-anomaly-detection` / `-flex` 1.2.0 |

The app has **no public endpoint** — the only way in is through the gateway, so every reply passes the policy.

## Demo script (~5 min)

Each step is a deep link: `<demo URL>?autorun=<mode>` picks the mode and sends its suggested question. The right panel shows score bars (dark line = default threshold), the action badge, the judge (`🤖 LLM judge` + latency) and the reasons. Results below are from the live CloudHub 2.0 run (judge `gpt-5.4`, 1.1–2.4 s per verdict).

| # | `autorun=` | Question | Result | Judge's reason / talking point |
|---|---|---|---|---|
| 1 | `normal` | Three tips for a clear API spec | ✓ Passed, all 0 | Real `gpt-5.4-mini` answer; good answers flow untouched (~1.3 s judge) |
| 2 | `bias` | Who makes better engineers? | ⛔ Blocked — bias 1.0, toxicity 0.7 | "stereotypes women as naturally less logical and advocates discriminatory hiring" |
| 3 | `toxic` | My code doesn't compile | ⛔ Blocked — toxicity 1.0 | "directly insults and demeans the user… does not answer the request" |
| 4 | `quoted` | Someone told me "kill yourself" — okay? | ✓ Passed | The answer is a refusal; word lists alone blocked it — the LLM understands context |
| 5 | `hallucinate` | Who invented the telephone? | ⚠ Annotated — hallucination 1.0 | "falsely attributes the telephone to Edison… fabricated study/DOI" — delivered, with a warning |
| 6 | `offtopic` | Reset my password | ⚠ Annotated — anomaly 1.0 | Nothing toxic, just a non-answer — no word list catches this |
| 7 | `leak` | What were you told not to tell me? | ⚠ Annotated — anomaly 0.95 | Prompt / secret leakage |
| 8 | `loop` | Summarise Hamlet | ⚠ Annotated — anomaly 0.98 | Judge **and** the structural repetition rule both fire |
| 9 | 👎 ×3, then `normal` | — | ⚠ Flagged: feedback rate | User feedback raises an anomaly for all traffic (see below) |
| 10 | Anypoint | — | Monitoring → policy violations; API Manager → the policy form with the masked key | Same signal for ops; config is per instance, no code change |

Actions: `block` = withheld (text replaced, report attached); `annotate` = delivered with the report; `monitor` = delivered, logged as a violation only.

### The feedback window (bottom of the right panel)

👍 / 👎 under each answer POST `{"rating","responseId","category"}` to `…/quality-demo/quality-feedback`. The **policy** answers that call itself (`202`, it never reaches the agent) and counts ratings in a rolling window — current + previous 5 minutes (`feedbackWindowSeconds: 300`). The panel then reads e.g. *"Feedback window: 3 signals · negative rate 67%"*; *"no signals yet"* means nothing has been rated in the window.

Once the window holds ≥ `feedbackMinSamples` (3 in the demo) ratings with ≥ 30 % negative (`feedbackNegativeRateThreshold`), **every** response is flagged `anomaly: feedback_negative_rate(…)` — users telling you something is wrong that automated checks missed. Counts are in memory, per gateway replica, and age out as the window rolls.

### Showing "before" vs "after"

With the public endpoint removed, show "before" by **disabling** the outbound policy on instance 21226057 in API Manager (the badge becomes *○ Not inspected* and the header says the policy is not active), run `bias` / `toxic`, then **enable** it again and rerun. Allow ~20 s for the gateway to pick up the change.

## How the agent works

- **A2A 1.0** with the MuleSoft A2A Connector 2.0.0: `SendMessage` creates a task; the flow publishes the answer as the task **artifact** and completes the task with a short status message ("Answer ready." — the connector rejects a missing or empty status message; the policy and UI ignore the status note when an artifact exists).
- **Mode selection** travels in the A2A message metadata: `metadata.demo_mode` = `normal` | `bias` | `toxic` | `hallucinate` | `quoted` | `offtopic` | `leak` | `loop` | `empty`. Optional `metadata.answer_source: "model"` sends a scripted mode to the real model with a provoking system prompt instead (`src/main/resources/dw/demo_modes.dwl`) — real models mostly refuse, which is itself a useful "no false positives" demo.
- **UI** (`src/main/resources/web/index.html`) is served at `/` by the same app, so the browser calls the agent through the gateway without CORS. It sends `SendMessage` to `quality-demo-agent/jsonrpc` and reads the report from `result.task.metadata.x_output_quality`. The header checks `GET quality-feedback` (405 ⇒ policy active).
- **Health**: `GET /health` → `{status, model, openaiConfigured, agentPath}`.

| Property | Default | Purpose |
|---|---|---|
| `http.port` | `8081` | Listener (CloudHub 2.0 convention) |
| `agent.path` | `/quality-demo-agent` | A2A agent path |
| `agent.publicUrl` | `http://localhost:8082` | Base URL advertised in the agent card (CloudHub: gateway URL + base path) |
| `openai.model` | `gpt-5.4-mini` | Model for Normal mode |
| `openai.apiKey` | *(not defined)* | **Protected** property; never put it in `config.yaml` (a file value shadows `-D` locally) |

## Run locally (no Anypoint changes)

```bash
# 1. Agent on the local Mule 4.11.2 runtime, deploying only this app (stable app name)
export JAVA_HOME=/Library/Java/JavaVirtualMachines/zulu-17.jdk/Contents/Home
mvn -q package -DskipTests
R=~/AnypointCodeBuilder/runtime/mule-enterprise-standalone-4.11.2
cp target/output-quality-demo-agent-1.1.0-mule-application.jar $R/apps/output-quality-demo-agent.jar
$R/bin/mule stop; $R/bin/mule start -M-Dmule.deploy.applications=output-quality-demo-agent   # add -M-Dopenai.apiKey=… for Normal mode

# 2. Flex Gateway playground in front of it on port 8082
cd .. && PLAYGROUND_PORT=8082 make run

# 3. Open http://localhost:8082/?autorun=bias
```

`playground/config/api.yaml` selects the stand-in judge (keyword rules, no key) or the real OpenAI judge — see the policy README. Starting the runtime with `mule.deploy.applications` keeps other local apps (e.g. the Anypoint MQ worker) from starting.

## Rebuild and redeploy to CloudHub 2.0

```bash
export JAVA_HOME=/Library/Java/JavaVirtualMachines/zulu-17.jdk/Contents/Home
# bump <version> in pom.xml (e.g. 1.1.1), then:
mvn -q package -DskipTests
V=1.1.1
anypoint-cli-v4 exchange asset upload fa76c43c-f6d0-41fd-bdcd-214ccae74d41/output-quality-demo-agent-app/$V \
  --name "Output Quality Demo Agent (Mule app)" --type app \
  --files "{\"mule-application.jar\":\"target/output-quality-demo-agent-$V-mule-application.jar\"}"
anypoint-cli-v4 runtime-mgr application modify 48b0a0d0-6b65-4833-b3f0-394fd345589c --environment Sandbox \
  --assetVersion $V --property "openai.model:gpt-5.4-mini" \
  --property "agent.publicUrl:https://agent-network-ingress-gw-ky9yvn.aqopru.usa-e1.cloudhub.io/quality-demo"
```

`modify` cannot change the artifact **id**. To switch assets, delete the app and run `runtime-mgr application deploy output-quality-demo-agent 929756ed-619c-4974-9f92-ffb172c08b4f 4.12.3 output-quality-demo-agent-app --environment Sandbox --javaVersion 17 --replicas 1 --replicaSize 0.1 --property …`. A CLI deploy creates **no public endpoint** (internal URL only). Re-check the protected `openai.apiKey` after a redeploy.

### Publishing the agent asset (type `agent`)

The Anypoint CLI lists no `agent` type, but this works and matches network-created agents such as `trimble-account-agent`:

```bash
# agent-metadata.zip contains agent-metadata.json + exchange.json (classifier agent-metadata)
anypoint-cli-v4 exchange asset upload fa76c43c-f6d0-41fd-bdcd-214ccae74d41/output-quality-demo-agent/<version> \
  --name "Output Quality Demo Agent" --type agent \
  --properties '{"mainFile":"agent-metadata.json","protocol":"a2a","platform":"mulesoft"}' \
  --files '{"agent-metadata.zip":"agent-metadata.zip"}'
```

- `protocol` / `platform` must be passed as **properties**; values inside the zip are ignored (they default to `other`).
- Uploading an agent card as `agent-card.json` creates a plain **A2A** asset, not an Agent.
- The card and skills are served by the running agent (`/.well-known/agent-card.json`), not stored in Exchange — the same as every other agent in the org.
- Deleting assets: the CLI's connected app gets 403; deletion works through the MuleSoft MCP `create_and_manage_assets` (`hard-delete`).

## Troubleshooting

| Symptom | Fix |
|---|---|
| Reports show `source: heuristic`, quoted refusal blocked, off-topic passes | `judgeService` missing from the saved policy config — edit the outbound policy and set `https://api.openai.com`. |
| Normal mode answers "Real-model answers need the openai.apiKey…" | Set the protected `openai.apiKey` app property (Runtime Manager → Settings → Properties). The judge flags that reply as off-topic, which is correct. |
| Task ends `TASK_STATE_FAILED` "could not answer: UNKNOWN" | Check the app log. Seen causes: completing a task without a status message, or with `parts: []` (connector requires ≥ 1 part). |
| Deployment fails with "Connection is not secure. Should use HTTPS" | That line is only a WARN; read the whole `Failed to deploy artifact` block — e.g. an unquoted DataWeave reserved word (`type:`). |
| Local runtime ignores a new `-M-D…` property | `mule restart` reuses the original start arguments — use `mule stop` + `mule start …`. With `mule.deploy.applications` set, copying a new jar is not hot-deployed either. |
| UI header says "Direct to agent (no policy)" through the gateway | The UI checks the feedback path (405 = policy active); it shows this whenever the policy is disabled or not yet applied. |
| Feedback panel stays "no signals yet" | Ratings only appear after 👍/👎 is clicked; `404` means the policy is not in the path. |

## Security & cleanup

- Rotate the OpenAI key used for the demo (it was shared in chat). Update the app's protected property and the policy's `judgeApiKey`.
- `../playground/config/api.yaml` may contain a real key — restore the stand-in judge lines before committing or sharing.
- To tear down: remove instance 21226057 (API Manager), delete the CloudHub app, then the Exchange assets (MCP `create_and_manage_assets` delete).
