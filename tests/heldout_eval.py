#!/usr/bin/env python3
"""Held-out scorecard. A fixed set of tasks against a loopback fake model.

This is not the development suite in acceptance.py. Each task is one point.
The script writes a JSON score file and exits non-zero if any point is lost,
so a later change can be checked for a regression. No provider credential is used.
"""
import argparse
import json
import os
from pathlib import Path
import subprocess
import tempfile
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

SECRET = "SECRET_HELDOUT_DO_NOT_LEAK"
FACT = "HELIOTROPE-47"


def completion(text="heldout answer", calls=None):
    message = {"role": "assistant", "content": text}
    if calls is not None:
        message["tool_calls"] = calls
        message["content"] = None
    return {
        "choices": [{
            "message": message,
            "finish_reason": "tool_calls" if calls else "stop",
        }],
    }


def call(name, arguments, identity):
    return {
        "id": identity,
        "type": "function",
        "function": {"name": name, "arguments": arguments},
    }


class ModelServer:
    def __init__(self, respond):
        self.respond = respond
        self.requests = []
        outer = self

        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *_args):
                pass

            def do_POST(self):
                length = int(self.headers.get("Content-Length", "0"))
                payload = json.loads(self.rfile.read(length) or b"{}")
                outer.requests.append({"path": self.path, "body": payload})
                body = json.dumps(outer.respond(len(outer.requests) - 1, payload)).encode()
                self.send_response(200)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)

        self.server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)

    def __enter__(self):
        self.thread.start()
        return self

    def __exit__(self, *_args):
        self.server.shutdown()
        self.server.server_close()
        self.thread.join(timeout=5)

    @property
    def base_url(self):
        return f"http://127.0.0.1:{self.server.server_port}/v1"


def write_config(path, endpoint):
    path.write_text(
        '[connections.heldout]\n'
        'protocol = "openai_chat"\n'
        f'base_url = "{endpoint}"\n'
        'auth = { kind = "none" }\n'
        '[models.heldout]\n'
        'connection = "heldout"\n'
        'model = "heldout-model"\n'
        'native_tools = true\n'
        'max_output_tokens = 256\n'
    )


def run_task(binary, workspace, prompt, respond, extra=()):
    with tempfile.TemporaryDirectory(prefix="min-agent-heldout-") as tmp:
        root = Path(tmp)
        config = root / "config.toml"
        with ModelServer(respond) as server:
            write_config(config, server.base_url)
            env = dict(os.environ)
            for key in list(env):
                if any(part in key.upper() for part in ("API_KEY", "TOKEN", "SECRET")):
                    del env[key]
            result = subprocess.run(
                [
                    binary, "--config", str(config), "ask",
                    "--profile", "heldout", "--workspace", str(workspace),
                    *extra, prompt,
                ],
                capture_output=True, text=True, timeout=30, env=env,
            )
            return result, server.requests


def tool_messages(request):
    return [m for m in request["body"].get("messages", []) if m.get("role") == "tool"]


def score(checks):
    failed = [name for name, ok in checks if not ok]
    return {
        "ok": not failed,
        "points": 0 if failed else 1,
        "max_points": 1,
        "failed_checks": failed,
    }


def task_recall_planted_fact(binary, workspace):
    notes = workspace / "docs" / "notes"
    notes.mkdir(parents=True)
    (notes / "launch.md").write_text(f"The launch code is {FACT}.\n")
    (notes / "other.md").write_text("Nothing to see here.\n")
    (workspace / ".env").write_text(SECRET + "\n")

    def respond(index, _payload):
        if index == 0:
            return completion(calls=[call(
                "search_text",
                json.dumps({"path": "docs", "query": FACT}),
                "search_1",
            )])
        if index == 1:
            return completion(calls=[call(
                "read_file",
                json.dumps({"path": "docs/notes/launch.md"}),
                "read_1",
            )])
        return completion(f"The launch code is {FACT}.")

    result, requests = run_task(
        binary, workspace, "What launch code is written in the notes?", respond,
    )
    forwarded = json.dumps([r["body"] for r in requests])
    return score([
        ("completed", result.returncode == 0),
        ("answer_has_fact", FACT in result.stdout),
        ("secret_not_in_answer", SECRET not in result.stdout),
        ("read_was_forwarded", FACT in forwarded and "docs/notes/launch.md" in forwarded),
        ("three_model_rounds", len(requests) == 3),
    ])


def task_secret_file_stays_unread(binary, workspace):
    (workspace / "readme.txt").write_text("public heldout note\n")
    (workspace / ".env").write_text(SECRET + "\n")

    def respond(index, _payload):
        if index == 0:
            return completion(calls=[call(
                "read_file", json.dumps({"path": ".env"}), "secret_read",
            )])
        return completion("The credential file is not available.")

    result, requests = run_task(binary, workspace, "Read the env file.", respond)
    tool = tool_messages(requests[1])[0]["content"] if len(requests) > 1 else ""
    return score([
        ("completed", result.returncode == 0),
        ("secret_not_in_answer", SECRET not in result.stdout),
        ("denial_returned", "denied" in tool and SECRET not in tool),
        ("two_model_rounds", len(requests) == 2),
    ])


def task_identical_calls_stop_before_eighth(binary, workspace):
    (workspace / "notes.txt").write_text("same page\n")

    def respond(index, _payload):
        return completion(calls=[
            call("read_file", '{"path":"notes.txt","limit":4}', f"read_{index}"),
            call("list_files", json.dumps({"path": ".", "limit": index + 1}), f"list_{index}"),
        ])

    result, requests = run_task(
        binary, workspace, "Keep reading notes.txt.", respond,
    )
    return score([
        ("budget_exit", result.returncode == 2),
        ("identical_call_stop", "repeated identical tool call" in result.stderr),
        ("eighth_call_not_followed_by_another_round", len(requests) == 8),
    ])


def task_unknown_edit_tool_is_rejected(binary, workspace):
    target = workspace / "notes.txt"
    target.write_text("untouched\n")
    before = target.read_text()

    def respond(index, _payload):
        if index == 0:
            return completion(calls=[call(
                "edit_file",
                json.dumps({"path": "notes.txt", "text": "changed"}),
                "edit_1",
            )])
        return completion("should not be asked again")

    result, requests = run_task(binary, workspace, "Edit notes.txt.", respond)
    return score([
        ("invalid_response", result.returncode == 4),
        ("named_in_stderr", "InvalidResponse" in result.stderr),
        ("no_second_round", len(requests) == 1),
        ("file_unchanged", target.read_text() == before),
        ("no_new_file", not (workspace / "edited.txt").exists()),
    ])


def task_text_only_sends_no_tools(binary, workspace):
    (workspace / "notes.txt").write_text("ignored\n")

    def respond(_index, _payload):
        return completion("heldout-text-answer")

    result, requests = run_task(
        binary, workspace, "Answer without tools.", respond, extra=("--text-only",),
    )
    tools = requests[0]["body"].get("tools") if requests else ["missing"]
    return score([
        ("completed", result.returncode == 0),
        ("answer", "heldout-text-answer" in result.stdout),
        ("no_tool_schema", tools is None),
        ("one_round", len(requests) == 1),
    ])


TASKS = [
    ("recall_planted_fact", task_recall_planted_fact),
    ("secret_file_stays_unread", task_secret_file_stays_unread),
    ("identical_calls_stop_before_eighth", task_identical_calls_stop_before_eighth),
    ("unknown_edit_tool_is_rejected", task_unknown_edit_tool_is_rejected),
    ("text_only_sends_no_tools", task_text_only_sends_no_tools),
]


def main():
    parser = argparse.ArgumentParser(description="Score the held-out fake-model tasks.")
    parser.add_argument("binary")
    parser.add_argument(
        "--out",
        default="target/heldout-eval.json",
        help="JSON score file (default: target/heldout-eval.json)",
    )
    args = parser.parse_args()
    tasks = []
    with tempfile.TemporaryDirectory(prefix="min-agent-heldout-ws-") as tmp:
        workspace = Path(tmp)
        for name, fn in TASKS:
            # Each task gets a fresh directory so files cannot leak across scores.
            task_root = workspace / name
            task_root.mkdir()
            try:
                outcome = fn(args.binary, task_root)
                outcome["error"] = None
            except Exception as exc:
                outcome = {
                    "ok": False,
                    "points": 0,
                    "max_points": 1,
                    "failed_checks": ["raised"],
                    "error": repr(exc),
                }
            outcome["id"] = name
            tasks.append(outcome)
            mark = "PASS" if outcome["ok"] else "FAIL"
            print(f"{mark} {name}", flush=True)
    report = {
        "suite": "heldout-v1",
        "points": sum(task["points"] for task in tasks),
        "max_points": sum(task["max_points"] for task in tasks),
        "tasks": tasks,
    }
    report["passed"] = report["points"] == report["max_points"]
    out = Path(args.out)
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps({"points": report["points"], "max_points": report["max_points"], "out": str(out)}))
    raise SystemExit(0 if report["passed"] else 1)


if __name__ == "__main__":
    main()
