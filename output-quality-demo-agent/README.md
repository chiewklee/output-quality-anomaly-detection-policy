# Output Quality Demo Agent

A Mule 4 **A2A 1.0 agent** (MuleSoft A2A Connector 2.0.0) built to demonstrate the [Output Quality & Anomaly Detection policy](../README.md). It answers with a real model (OpenAI `gpt-5.4-mini`) or, on request, with scripted misbehaving answers — biased, toxic, hallucinated, off-topic, prompt-leaking, looping — so the policy has something to catch. It also serves the demo UI.

- Runbook, deployed resources, demo script and troubleshooting: **[DEMO.md](DEMO.md)**
- Runtime: Mule 4.12.3 (CloudHub 2.0, Edge channel), `minMuleVersion` 4.11.0, Java 17
- Flows: `src/main/mule/demo-agent.xml` — A2A server + task listener, UI (`/`), health (`/health`)
- Scripted answers and real-model provocation prompts: `src/main/resources/dw/demo_modes.dwl`
- UI: `src/main/resources/web/index.html`

```bash
export JAVA_HOME=/Library/Java/JavaVirtualMachines/zulu-17.jdk/Contents/Home
mvn -q package -DskipTests   # → target/output-quality-demo-agent-<version>-mule-application.jar
```

Example call (through the gateway):

```bash
curl -s https://agent-network-ingress-gw-ky9yvn.aqopru.usa-e1.cloudhub.io/quality-demo/quality-demo-agent/jsonrpc \
  -H 'content-type: application/json' -H 'a2a-version: 1.0' \
  -d '{"jsonrpc":"2.0","id":1,"method":"SendMessage","params":{"message":{"messageId":"m1","role":"ROLE_USER",
       "parts":[{"text":"Who invented the telephone?"}],"metadata":{"demo_mode":"hallucinate"}}}}'
# → result.task.artifacts[0].parts[0].text + result.task.metadata.x_output_quality
```
