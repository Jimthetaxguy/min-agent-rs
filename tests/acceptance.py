#!/usr/bin/env python3
"""Independent black-box checks against a built min-agent executable.

Only temporary files and a loopback fake model are used. Config/CLI adaptation
belongs in write_config and invocation; no provider credential is consumed.
"""
import argparse
import json
import os
from pathlib import Path
import subprocess
import tempfile
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer


def completion(text="fixture complete", calls=None, reason=None, **extra):
    message = {"role": "assistant", "content": text}
    if calls is not None:
        message["tool_calls"] = calls
    message.update(extra)
    return {
        "id": "fixture-response",
        "object": "chat.completion",
        "choices": [{
            "index": 0,
            "message": message,
            "finish_reason": reason or ("tool_calls" if calls else "stop"),
        }],
    }


def call(name="read_file", arguments=None, identity="call_1"):
    return {
        "id": identity,
        "type": "function",
        "function": {
            "name": name,
            "arguments": arguments if arguments is not None else '{"path":"hello.txt"}',
        },
    }


class ModelServer:
    def __init__(self, responses):
        self.responses = responses
        self.requests = []
        outer = self

        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *_args):
                pass

            def do_POST(self):
                length = int(self.headers.get("Content-Length", "0"))
                payload = json.loads(self.rfile.read(length))
                outer.requests.append({
                    "path": self.path,
                    "headers": dict(self.headers),
                    "body": payload,
                })
                index = len(outer.requests) - 1
                response = outer.responses[min(index, len(outer.responses) - 1)]
                data = json.dumps(response).encode()
                self.send_response(200)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(data)))
                self.end_headers()
                self.wfile.write(data)

        self.server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)

    def __enter__(self):
        self.thread.start()
        return self

    def __exit__(self, *_args):
        self.server.shutdown()
        self.server.server_close()
        self.thread.join()

    @property
    def base_url(self):
        return f"http://127.0.0.1:{self.server.server_port}/v1"


def write_config(path, endpoint, text_only=False, **_options):
    # Adapt to the actual package schema before first execution.
    path.write_text(
        '[connections.fixture]\n'
        'protocol = "openai_chat"\n'
        f'base_url = "{endpoint}"\n'
        'auth = { kind = "none" }\n'
        '[models.fixture]\n'
        'connection = "fixture"\n'
        'model = "fixture-model"\n'
        f'native_tools = {str(not text_only).lower()}\n'
    )


def invocation(binary, config, workspace, text_only=False):
    args = [
        binary, "--config", str(config), "ask",
        "--profile", "fixture", "--workspace", str(workspace),
    ]
    if text_only:
        args += ["--text-only"]
    return args + ["Read hello.txt and report its contents."]


def run_case(binary, responses, text_only=False, setup=None):
    with tempfile.TemporaryDirectory(prefix="min-agent-accept-") as tmp:
        root = Path(tmp)
        workspace = root / "workspace"
        workspace.mkdir()
        (workspace / "hello.txt").write_text("UNIQUE_FIXTURE_CONTENT\n")
        (workspace / ".env").write_text("SECRET_FIXTURE_MUST_NOT_LEAVE\n")
        (root / "outside.txt").write_text("OUTSIDE_FIXTURE_MUST_NOT_LEAVE\n")
        if setup:
            setup(root, workspace)
        with ModelServer(responses) as server:
            config = root / "config.toml"
            write_config(config, server.base_url, text_only=text_only)
            env = dict(os.environ)
            # Avoid accidental inherited provider use even if the CLI is wrong.
            for key in list(env):
                if any(s in key.upper() for s in ("API_KEY", "TOKEN", "SECRET")):
                    del env[key]
            result = subprocess.run(
                invocation(binary, config, workspace, text_only),
                capture_output=True, text=True, timeout=25, env=env,
            )
            return result, server.requests


def assert_ok(result):
    assert result.returncode == 0, (result.returncode, result.stdout, result.stderr)


def assert_rejected(result, requests):
    assert result.returncode != 0, (result.stdout, result.stderr)
    assert len(requests) == 1, f"Rejected model batch triggered {len(requests)} requests"


def test_round_trip(binary):
    result, requests = run_case(binary, [
        completion(None, [call()], reasoning_content="opaque-fixture"),
        completion(),
    ])
    assert_ok(result)
    assert len(requests) == 2
    assert requests[0]["path"] == "/v1/chat/completions"
    assert requests[0]["body"]["model"] == "fixture-model"
    assert requests[0]["body"].get("tools")
    assert not requests[0]["headers"].get("Authorization")
    history = requests[1]["body"]["messages"]
    tool_results = [m for m in history if m.get("role") == "tool"]
    assert len(tool_results) == 1
    assert tool_results[0]["tool_call_id"] == "call_1"
    assert "UNIQUE_FIXTURE_CONTENT" in tool_results[0]["content"]
    assistant = next(m for m in history if m.get("tool_calls"))
    assert assistant["reasoning_content"] == "opaque-fixture"


def test_text_only(binary):
    result, requests = run_case(binary, [completion()], text_only=True)
    assert_ok(result)
    assert len(requests) == 1
    assert not requests[0]["body"].get("tools")
    assert "tool_choice" not in requests[0]["body"]


def test_malformed_arguments(binary):
    result, requests = run_case(binary, [completion(None, [call(arguments="{")])])
    assert_rejected(result, requests)


def test_array_arguments(binary):
    result, requests = run_case(binary, [completion(None, [call(arguments="[]")])])
    assert_rejected(result, requests)


def test_duplicate_ids(binary):
    result, requests = run_case(binary, [completion(None, [call(), call()])])
    assert_rejected(result, requests)


def test_truncated_call(binary):
    result, requests = run_case(binary, [
        completion(None, [call()], reason="length"),
    ])
    assert_rejected(result, requests)


def test_unknown_tool(binary):
    result, requests = run_case(binary, [completion(None, [call(name="run_shell")])])
    assert_rejected(result, requests)


def assert_no_leak(requests):
    assert requests, "No model request was made; configuration may be wrong"
    payloads = json.dumps(requests)
    assert "SECRET_FIXTURE_MUST_NOT_LEAVE" not in payloads
    assert "OUTSIDE_FIXTURE_MUST_NOT_LEAVE" not in payloads


def test_path_traversal(binary):
    result, requests = run_case(binary, [
        completion(None, [call(arguments='{"path":"../outside.txt"}')]),
        completion(),
    ])
    assert_rejected(result, requests)
    assert_no_leak(requests)


def test_secret_path(binary):
    result, requests = run_case(binary, [
        completion(None, [call(arguments='{"path":".env"}')]),
        completion(),
    ])
    assert_rejected(result, requests)
    assert_no_leak(requests)


def test_symlink(binary):
    result, requests = run_case(
        binary,
        [completion(None, [call(arguments='{"path":"escape.txt"}')]), completion()],
        setup=lambda root, workspace: (workspace / "escape.txt").symlink_to(root / "outside.txt"),
    )
    assert_rejected(result, requests)
    assert_no_leak(requests)


def test_repetition_budget(binary):
    result, requests = run_case(binary, [
        completion(None, [call(identity=f"call_{i}")]) for i in range(10)
    ])
    assert result.returncode != 0, "Repeated tools incorrectly completed"
    assert len(requests) == 3, len(requests)
    assert "repeated" in result.stderr.lower(), result.stderr


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("binary")
    args = parser.parse_args()
    tests = [v for k, v in globals().items() if k.startswith("test_") and callable(v)]
    failed = []
    for test in tests:
        try:
            test(args.binary)
            print(f"PASS {test.__name__}", flush=True)
        except Exception as exc:
            failed.append(test.__name__)
            print(f"FAIL {test.__name__}: {exc!r}", flush=True)
    print(json.dumps({"passed": len(tests) - len(failed), "failed": failed}))
    raise SystemExit(bool(failed))


if __name__ == "__main__":
    main()
