"""A scripted model for the assistant, for the screenshots and the demo video.

It speaks the OpenAI chat API on 127.0.0.1 and plays one exchange: asked a
question, it lists the follow-up and promotions mailboxes through the app's
own tools, then answers from what they hold. The app runs those tools
against the demo store, so the pane shows real tool steps.
"""

import http.server
import json
import re
import threading
import time

QUESTION = "Which sent mail is waiting on a reply, and what is in my promotions?"

# What the scripted model says once the app has answered its two tool
# calls. It describes the demo's own sample mail. The demo dates its mail
# relative to now, and so does this.
ANSWER = """**Waiting on a reply**

1. **Invoice 2291 for August**, to Owen Mercer 5 days ago. You asked him to confirm it reached the right person, with the invoice attached.
2. **Question about the lease renewal**, to Harbor Lane Lettings 8 days ago. You asked to renew for another twelve months at the current rent.

**In Promotions**

1. **Trail Notes**: five autumn loops under 15 km, and a gear list for cold mornings.
2. **Linden Books**: 20% off travel guides until Sunday night with the code WANDER. You have not opened it yet."""

REASONING = """The person wants two things: sent mail still waiting on an answer, and what sits in Promotions. The follow_up mailbox lists sent mail that has waited three days or more, and the inbox with the promotions category covers the second. Both calls can go out together."""

MODEL = "scripted-demo"

# Seconds between streamed words. The demo video slows this to reading pace.
PACE = 0.01


class ScriptedModel(http.server.BaseHTTPRequestHandler):
    """An OpenAI-compatible server that plays one exchange. Asked a
    question, it lists the follow-up and promotions mailboxes through the
    app's own tools, then gives the answer above. The app runs those tools
    against the demo store, so the pane shows its real tool steps."""

    def log_message(self, *args):
        pass

    def do_GET(self):
        data = json.dumps({"data": [{"id": MODEL}]}).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def do_POST(self):
        length = int(self.headers.get("Content-Length", 0))
        request = json.loads(self.rfile.read(length) or b"{}")
        messages = request.get("messages", [])
        if messages and messages[-1].get("role") == "tool":
            # Streamed a word at a time, as a model would.
            chunks = [{"content": piece} for piece in re.findall(r"\S+\s*", ANSWER)]
        else:
            calls = [
                ("list_mail", {"mailbox": "follow_up"}),
                ("list_mail", {"mailbox": "inbox", "category": "promotions"}),
            ]
            # It thinks first, the way LM Studio streams a reasoning model.
            chunks = [
                {"reasoning_content": piece} for piece in re.findall(r"\S+\s*", REASONING)
            ] + [
                {
                    "tool_calls": [
                        {
                            "index": index,
                            "id": "call-%d" % index,
                            "type": "function",
                            "function": {"name": name, "arguments": json.dumps(arguments)},
                        }
                    ]
                }
                for index, (name, arguments) in enumerate(calls)
            ]
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.end_headers()
        for delta in chunks:
            chunk = {"choices": [{"index": 0, "delta": delta}]}
            self.wfile.write(b"data: " + json.dumps(chunk).encode() + b"\n\n")
            self.wfile.flush()
            time.sleep(PACE)
        self.wfile.write(b"data: [DONE]\n\n")


def scripted_model():
    """Starts the scripted model on a free port and returns its address."""
    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), ScriptedModel)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    return "http://127.0.0.1:%d/v1" % server.server_address[1]
