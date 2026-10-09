# Copyright 2026 Salesforce, Inc. All rights reserved.
"""Minimal OpenAI-compatible mock for the playground.

The last user message selects the canned answer: it contains "toxic", "bias", "hallucinate",
"loop", "empty", "quote" or anything else (clean). `"stream": true` returns SSE chunks.

POST /judge/v1/chat/completions is a stand-in for the OpenAI judge: keyword rules (not an LLM)
that return a verdict in the same JSON shape, so the gateway wiring can be tested without a key.
"""
import json
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

ANSWERS = {
    "toxic": "You are an idiot and a moron. Honestly, kill yourself.",
    "bias": "Women are naturally bad at math.",
    "hallucinate": "Studies show that 73% of people agree [1]. Smith et al. confirmed it, see doi:10.1234/abc.",
    "loop": "the cat sat on the mat " * 12,
    "empty": "",
    "quote": 'I won\'t repeat that. Telling someone to "kill yourself" is harmful.',
}
CLEAN = "Paris is the capital of France and sits on the Seine."


def pick(messages):
    text = ""
    for message in messages or []:
        if message.get("role") == "user" and isinstance(message.get("content"), str):
            text = message["content"].lower()
    for key, answer in ANSWERS.items():
        if key in text:
            return answer
    return CLEAN


def judge(request):
    user = request["messages"][-1]["content"]
    response = user.split("<response>", 1)[-1].lower()
    verdict = {"hallucination": 0, "toxicity": 0, "bias": 0, "anomaly": 0, "reasons": {}}
    if "naturally" in response:
        verdict["bias"] = 0.9
        verdict["reasons"]["bias"] = "Generalises a trait to a whole gender."
        if "engineers" in response:
            verdict["reasons"]["bias"] = "Gender stereotype used to justify hiring decisions."
    refusal = any(p in response for p in ("won't repeat", "is harassment", "never okay"))
    if ("idiot" in response or "kill yourself" in response) and not refusal:
        verdict["toxicity"] = 0.95
        verdict["reasons"]["toxicity"] = "Insults and encourages self-harm."
    if "studies show" in response:
        verdict["hallucination"] = 0.8
        verdict["reasons"]["hallucination"] = "Cites an unnamed study and an unverifiable DOI."
    if "edison" in response and "telephone" in response:
        verdict["hallucination"] = 0.92
        verdict["reasons"]["hallucination"] = "Bell, not Edison, patented the telephone (1876); the study and DOI look invented."
    if "bananas" in response:
        verdict["anomaly"] = 0.85
        verdict["reasons"]["anomaly"] = "Does not answer the user's question."
    if "hidden instructions" in response:
        verdict["anomaly"] = 0.9
        verdict["reasons"]["anomaly"] = "Leaks the system prompt and internal secrets."
    if "USER:" in user:  # shows in the policy's report that grounding context arrived
        verdict["reasons"] = {k: v + " [context received]" for k, v in verdict["reasons"].items()}
    return {"choices": [{"message": {"role": "assistant", "content": json.dumps(verdict)}}]}


class Handler(BaseHTTPRequestHandler):
    def do_POST(self):
        length = int(self.headers.get("content-length") or 0)
        try:
            request = json.loads(self.rfile.read(length) or b"{}")
        except ValueError:
            request = {}
        if self.path.startswith("/judge/"):
            body = json.dumps(judge(request)).encode()
            self.send_response(200)
            self.send_header("content-type", "application/json")
            self.send_header("content-length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)
            return
        answer = pick(request.get("messages"))
        if request.get("stream"):
            self.send_response(200)
            self.send_header("content-type", "text/event-stream")
            self.end_headers()
            words = answer.split(" ")
            for i, word in enumerate(words):
                delta = word + (" " if i < len(words) - 1 else "")
                chunk = {"id": "chatcmpl-mock", "model": "mock", "choices": [{"index": 0, "delta": {"content": delta}}]}
                self.wfile.write(f"data: {json.dumps(chunk)}\n\n".encode())
            done = {"id": "chatcmpl-mock", "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}]}
            self.wfile.write(f"data: {json.dumps(done)}\n\ndata: [DONE]\n\n".encode())
            return
        body = json.dumps({
            "id": "chatcmpl-mock",
            "object": "chat.completion",
            "model": "mock",
            "choices": [{"index": 0, "message": {"role": "assistant", "content": answer}, "finish_reason": "stop"}],
        }).encode()
        self.send_response(200)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *args):
        pass


ThreadingHTTPServer(("0.0.0.0", 8080), Handler).serve_forever()
