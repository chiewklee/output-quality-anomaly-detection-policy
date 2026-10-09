# Output Quality & Anomaly Detection Policy

A Flex / Omni Gateway custom policy (PDK 1.10) that scores **LLM and A2A-agent responses** for **hallucination**, **toxicity**, **bias** and **abnormal output**, and lets you `monitor`, `annotate` or `block` each category independently. It is an **outbound** policy: it sits on the gateway-to-upstream leg and judges exactly what the agent or model returned.

| Layer | What it catches | When it runs |
|---|---|---|
| **LLM judge** (OpenAI or any OpenAI-compatible API) | meaning: fabricated or ungrounded claims (it sees the user's question), insults, stereotypes, off-topic or non-answers, leaked prompts, your own `judgeInstructions` | every response when `judgeService` is set |
| Word-list heuristics | threats, insults, profanity, group generalisations, unsupported citations, hedging | fallback when no judge is set or it fails |
| Structural signals | low token confidence, empty / truncated / looping / garbled output, failed A2A tasks, length outliers, spikes in negative user feedback | always, combined with either of the above |

| | |
|---|---|
| Exchange assets | `output-quality-anomaly-detection` (definition) + `output-quality-anomaly-detection-flex` (implementation), group `fa76c43c-f6d0-41fd-bdcd-214ccae74d41` |
| Current version | **1.2.0** |
| Applies to | `a2a`, `a2a_v1`, `llm`, `http` instances |
| Injection point | outbound |
| Traffic understood | A2A Legacy + v1.0 (JSON-RPC and HTTP+JSON, incl. streaming), OpenAI Chat Completions / Completions / Responses, Anthropic Messages (JSON + SSE) |
| Full behaviour | [`docs/spec.md`](docs/spec.md) · schema [`definition/gcl.yaml`](definition/gcl.yaml) |

### Versions

| Version | Change |
|---|---|
| 1.0.0 | LLM APIs, word lists + OpenAI moderation judge |
| 1.1.0 | LLM judge (chat completions), A2A support, `reportAll`, asset types `a2a`/`a2a_v1` |
| 1.2.0 | Outbound injection point |

## Applying it in Anypoint API Manager

1. Open the API / agent instance (e.g. an A2A agent on `agent-network-ingress-gw`) → **Outbound policies** → **Add policy** → *Output Quality & Anomaly Detection*.
2. Fill in the form. Typical demo configuration:

| Field | Value | Notes |
|---|---|---|
| `judgeService` | `https://api.openai.com` | **Required for the LLM judge.** Empty → word lists only. Confirm it is saved (see Troubleshooting). |
| `judgeModel` | `gpt-5.4` | Use a different model from the agent's own (the agent uses `gpt-5.4-mini`) so it is not marking its own homework. |
| `judgeApiKey` | OpenAI key | Secret field, masked and never logged. |
| `toxicityAction` | `block` | Harmful answers never reach the caller. |
| `biasAction` / `hallucinationAction` / `anomalyAction` | `annotate` | Delivered with a report attached. |
| `reportAll` | `true` | Clean answers also carry scores (for the demo UI). |
| `feedbackMinSamples` | `3` | So the feedback anomaly can be shown live. |

Every flagged response is recorded as a policy violation in Anypoint Monitoring and logged by the gateway.

### What callers see

- **A2A** — the report is in the task metadata: `result.task.metadata.x_output_quality` (`flagged`, `action`, `source`, `judgeMs`, `scores`, `reasons`). A blocked task keeps its ids and state, and every text part reads *"This response was withheld because it did not meet output quality requirements."*
- **LLM APIs** — the report is a top-level `x_output_quality` field; a blocked Chat Completion has the withheld text and `finish_reason: content_filter`.
- The HTTP status is never changed (response headers are already sent when the body is judged).
- `POST <base-path>/quality-feedback` with `{"rating":"positive"|"negative","responseId":"…","category":"…"}` is answered by the policy (`202`) and feeds the negative-feedback anomaly.

### Troubleshooting

| Symptom | Cause / fix |
|---|---|
| Reports say `source: "heuristic"`, `judgeMs: null` | `judgeService` is not in the saved configuration. Edit the policy and set it; verify with the API Manager config (or `list_api_instances` with applied policies). |
| `source: "heuristic_fallback"` | The judge was called and failed (bad key → 401, rate limit → 429, timeout). Check the gateway log line `judge returned status …`. |
| No report at all | Not 2xx, compressed with `stripAcceptEncoding: false`, an A2A method other than send (e.g. `GetTask`), or the request bypassed the gateway. |
| Publish fails: `assetTypes/<i> must be equal to one of the allowed values` | Exchange spells A2A v1 as `a2a_v1` (not `a2av1`). |
| `make build` fails in `gcl-gen` | Install `cargo-anypoint@1.10.0` (`make setup`). |

## Build, test, release

```bash
make setup                  # cargo-anypoint 1.10.0 + llvm-cov
cargo test --lib            # 102 unit + pdk-unit end-to-end tests (mocked judge, A2A + LLM)
make test                   # Docker integration test on Flex Gateway 1.13.0
make build                  # WASM + GCL
make release                # publish definition + implementation to Exchange (bump Cargo.toml version first)
```

## Local playground

`make run` starts Flex Gateway 1.13.0 in Docker with the policy and the config in `playground/config/api.yaml`. `PLAYGROUND_PORT=8082 make run` exposes it on 8082 (use this when the demo agent runs locally on 8081).

The upstream in `api.yaml` is currently the **local demo agent** (`http://host.docker.internal:8081`, see `../output-quality-demo-agent/DEMO.md`). Two judge options are in the file:

- **Stand-in judge** (no key): `judgeService: http://backend:8080`, `judgePath: /judge/v1/chat/completions` — keyword rules in `playground/mock-llm/server.py`; reasons end with `[context received]`. Not an LLM.
- **Real OpenAI judge**: `judgeService: https://api.openai.com`, `judgePath: /v1/chat/completions`, `judgeModel`, `judgeApiKey`. **Never commit `api.yaml` with a real key.**

The `backend` container (`playground/mock-llm/server.py`) also serves a mock OpenAI-compatible LLM (`POST /v1/chat/completions`, answer chosen by keywords `toxic`, `bias`, `hallucinate`, `loop`, `empty`, `quote`; `"stream": true` for SSE) — point the upstream at `http://backend:8080` to test the LLM-API path.

---

## PDK reference

This policy was created with the Flex Gateway Policy Development Kit (PDK). To find the complete PDK documentation, see [PDK Overview](https://docs.mulesoft.com/pdk/latest/policies-pdk-overview) on the Mulesoft documentation site.


## Make command reference
This project has a Makefile that includes different goals that assist the developer during the policy development lifecycle.

*For more information about the Makefile, see [Makefile](https://docs.mulesoft.com/pdk/latest/policies-pdk-create-project#makefile).*

### Setup
The `make setup` goal installs the Policy Development Kit internal dependencies for the rest of the Makefile goals.

*For more information about `make setup`, see [Setup the PDK Build environment](https://docs.mulesoft.com/pdk/latest/policies-pdk-create-project#setup-the-pdk-build-environment).*

### Build asset files
The `make build-asset-files` goal generates all the policy asset files required to build, execute, and publish the policy. This command also updates the `config.rs` source code file with the latest configurations defined in the policy definition.

*For more information about creating a policy definition, see [Defining a Policy Schema Definition](https://docs.mulesoft.com/pdk/latest/policies-pdk-create-schema-definition).*

*For more information about `make build-asset-files`, see [Compiling Custom Policies](https://docs.mulesoft.com/pdk/latest/policies-pdk-compile-policies).*

### Build
The `make build` goal compiles the WebAssembly binary of the policy.
Since the source code must be in sync with the policy definition configurations, this goal runs the `build-asset-files` before compiling.

*For more information about `make build`, see [Compiling Custom Policies](https://docs.mulesoft.com/pdk/latest/policies-pdk-compile-policies).*

### Run
The `make run` goal provides a simple way to execute the current build of the policy in a Docker containerized environment. In order to run this goal, the `playground/config` directory must contain a set of files required for executing the policy in a Flex Gateway instance:
- A `registration.yaml` file generated by performing a Flex Gateway registration in Local Mode. If you already have an instance registered in Local mode, you can reuse the registration file you have and copy it in the `playground/config` folder.
Otherwise, to complete the registration we recommend using the Anypoint Platform:
    1. Go to `Runtime Manager`
    2. Navigate to the `Flex Gateway` tab
    3. Click the `Add Gateway` button
    4. Select `Docker` as your OS and copy the registration command replacing `--connected=true` to `--connected=false`.
    5. Paste the command and run it in the `playground/config` directory.

- An `api.yaml` file updated with the desired policy configuration. This file also supports adding other policies to be applied along the one being developed.

The `playground/config` directory can also contain other resource definitions, such as accessory services used by the policy (Eg. a remote authentication service).

*For more information about `make run`, see [Debugging Custom Policies Locally with PDK](https://docs.mulesoft.com/pdk/latest/policies-pdk-debug-local).*

### Test
The `make test` goal runs unit tests and integration tests. Integration tests are placed in the `tests` directory and are configured with the files placed at the
`tests/<module-name>/<test-name>` directory.

*For more information about writing integration tests, see [Writing Integration Tests](https://docs.mulesoft.com/pdk/latest/policies-pdk-integration-tests).*

### Publish
The `make publish` goal publishes the policy asset in Anypoint Exchange, in your configured Organization.

Since the publish goal is intended to publish a policy asset in development, the _assetId_ and name published will explicitly say `dev`, and the versions published will include a timestamp at the end of the version. Eg.
- groupId: your configured organization id
- visible name: _{Your policy name} Dev_
- assetId: _{your-policy-asset-id}-dev_
- version: _{your-policy-version}-20230618115723_

*For more information about publishing policies, see [Uploading Custom Policies to Exchange](https://docs.mulesoft.com/pdk/latest/policies-pdk-publish-policies).*

### Release
The `make release` goal also publishes the policy to Anypoint Exchange, but as a ready for production asset. In this case, the groupId, visible name, assetId and version will be the ones defined in the project.

*For more information about releasing policies, see [Uploading Custom Policies to Exchange](https://docs.mulesoft.com/pdk/latest/policies-pdk-publish-policies).*

### Skipping unchanged definition publishes
Both `make publish` and `make release` accept the `SKIP_UNCHANGED_DEFINITION` variable (default `true`). When enabled, the definition is not republished if its content matches the version already published in Exchange; instead the implementation asset is published with its dependency pointing at that already-published definition version. Set `SKIP_UNCHANGED_DEFINITION=false` to always republish the definition:

```
make publish SKIP_UNCHANGED_DEFINITION=false
```


### Policy Examples

The PDK provides provides a set of example policy projects to get started creating policies and using the PDK features. To learn more about these examples see [Custom policy Examples](https://docs.mulesoft.com/pdk/latest/policies-pdk-policy-templates).
