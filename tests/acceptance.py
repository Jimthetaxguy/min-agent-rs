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
import time
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
                    # HTTP header names are case-insensitive; normalize so lookups are real.
                    "headers": {k.lower(): v for k, v in self.headers.items()},
                    "body": payload,
                })
                index = len(outer.requests) - 1
                response = outer.responses[min(index, len(outer.responses) - 1)]
                if callable(response):
                    response(self)
                    return
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


def write_config(path, endpoint, text_only=False, protocol="openai_chat", auth=None):
    auth = auth or '{ kind = "none" }'
    path.write_text(
        '[connections.fixture]\n'
        f'protocol = "{protocol}"\n'
        f'base_url = "{endpoint}"\n'
        f'auth = {auth}\n'
        '[models.fixture]\n'
        'connection = "fixture"\n'
        'model = "fixture-model"\n'
        f'native_tools = {str(not text_only).lower()}\n'
        'max_output_tokens = 256\n'
    )


def invocation(binary, config, workspace, text_only=False, extra=()):
    args = [
        binary, "--config", str(config), "ask",
        "--profile", "fixture", "--workspace", str(workspace),
    ]
    if text_only:
        args += ["--text-only"]
    return args + list(extra) + ["Read hello.txt and report its contents."]


def run_case(binary, responses, text_only=False, setup=None, protocol="openai_chat",
             auth=None, env_extra=None, extra=(), inspect=None):
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
            write_config(config, server.base_url, text_only=text_only,
                         protocol=protocol, auth=auth)
            env = dict(os.environ)
            # Avoid accidental inherited provider use even if the CLI is wrong.
            for key in list(env):
                if any(s in key.upper() for s in ("API_KEY", "TOKEN", "SECRET")):
                    del env[key]
            env.update(env_extra or {})
            extra = [a.replace("{root}", str(root)) for a in extra]
            result = subprocess.run(
                invocation(binary, config, workspace, text_only, extra),
                capture_output=True, text=True, timeout=25, env=env,
            )
            if inspect:
                inspect(root)
            return result, server.requests


def assert_ok(result):
    assert result.returncode == 0, (result.returncode, result.stdout, result.stderr)


def assert_rejected(result, requests):
    """Protocol violation: nothing executes, no second request, exit code 4."""
    assert result.returncode == 4, (result.returncode, result.stdout, result.stderr)
    assert len(requests) == 1, f"Rejected model batch triggered {len(requests)} requests"
    assert "InvalidResponse" in result.stderr, result.stderr


def assert_denied_then_recovered(result, requests, kind="denied"):
    """Environment answer: the denial is fed back as a tool error and the run completes."""
    assert_ok(result)
    assert len(requests) == 2, len(requests)
    tool_results = [m for m in requests[1]["body"]["messages"] if m.get("role") == "tool"]
    assert len(tool_results) == 1
    error = json.loads(tool_results[0]["content"])["error"]
    assert error["kind"] == kind, error


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
    assert "authorization" not in requests[0]["headers"]
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
    assert_denied_then_recovered(result, requests)
    assert_no_leak(requests)


def test_secret_path(binary):
    result, requests = run_case(binary, [
        completion(None, [call(arguments='{"path":".env"}')]),
        completion(),
    ])
    assert_denied_then_recovered(result, requests)
    assert_no_leak(requests)


def test_symlink(binary):
    result, requests = run_case(
        binary,
        [completion(None, [call(arguments='{"path":"escape.txt"}')]), completion()],
        setup=lambda root, workspace: (workspace / "escape.txt").symlink_to(root / "outside.txt"),
    )
    assert_denied_then_recovered(result, requests)
    assert_no_leak(requests)


def test_repetition_budget(binary):
    result, requests = run_case(binary, [
        completion(None, [call(identity=f"call_{i}")]) for i in range(10)
    ])
    assert result.returncode == 2, "Repeated tools incorrectly completed"
    assert len(requests) == 3, len(requests)
    assert "repeated" in result.stderr.lower(), result.stderr


def raw(data):
    """A response handler that writes raw bytes, bypassing well-formed HTTP framing."""
    def handler(request):
        request.close_connection = True
        request.wfile.write(data)
        request.wfile.flush()
    return handler


def test_missing_file_recovers(binary):
    result, requests = run_case(binary, [
        completion(None, [call(arguments='{"path":"nope.txt"}')]),
        completion(),
    ])
    assert_denied_then_recovered(result, requests, kind="not_found")


def test_same_error_streak_stops(binary):
    result, requests = run_case(binary, [
        completion(None, [call(arguments=f'{{"path":"nope{i}.txt"}}', identity=f"c{i}")])
        for i in range(6)
    ])
    assert result.returncode == 2, (result.returncode, result.stderr)
    assert "repeated tool error" in result.stderr, result.stderr
    assert len(requests) == 3, len(requests)


def test_fifo_is_refused_without_blocking(binary):
    result, requests = run_case(
        binary,
        [completion(None, [call(arguments='{"path":"pipe"}')]), completion()],
        setup=lambda root, workspace: os.mkfifo(workspace / "pipe"),
    )
    assert_denied_then_recovered(result, requests, kind="wrong_type")


def test_control_character_file_fits(binary):
    def setup(root, workspace):
        (workspace / "nul.bin").write_bytes(b"\x00" * 8192)
    result, requests = run_case(
        binary,
        [completion(None, [call(arguments='{"path":"nul.bin"}')]), completion()],
        setup=setup,
    )
    assert_ok(result)
    tool = [m for m in requests[1]["body"]["messages"] if m.get("role") == "tool"][0]
    content = json.loads(tool["content"])
    assert len(tool["content"]) <= 32768
    assert content["truncated"] is True and content["next_offset"] < 8192


def test_known_credentials_are_redacted(binary):
    def setup(root, workspace):
        (workspace / "settings.py").write_text("AWS = 'AKIAABCDEFGHIJKLMNOP'\n")
    result, requests = run_case(
        binary,
        [completion(None, [call(arguments='{"path":"settings.py"}')]), completion()],
        setup=setup,
    )
    assert_ok(result)
    assert "AKIAABCDEFGHIJKLMNOP" not in json.dumps(requests)
    assert "[REDACTED]" in json.dumps(requests)


def assert_provider_stop(result, requests, fragment):
    assert result.returncode == 3, (result.returncode, result.stderr)
    assert fragment in result.stderr, result.stderr
    assert len(requests) == 1, len(requests)


def test_chunked_oversize_body(binary):
    chunk = b"a" * 65536
    body = b"".join(b"%x\r\n%s\r\n" % (len(chunk), chunk) for _ in range(20)) + b"0\r\n\r\n"
    handler = raw(b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n"
                  b"Transfer-Encoding: chunked\r\nConnection: close\r\n\r\n" + body)
    result, requests = run_case(binary, [handler], extra=["--max-retries", "0"])
    assert_provider_stop(result, requests, "response exceeds byte limit")


def test_slow_body_hits_request_deadline(binary):
    def handler(request):
        request.close_connection = True
        request.wfile.write(b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n"
                            b"Content-Length: 100000\r\n\r\n{\"choices\":")
        request.wfile.flush()
        time.sleep(4)
    started = time.monotonic()
    result, requests = run_case(binary, [handler],
                                extra=["--request-timeout-secs", "1", "--max-retries", "0"])
    assert time.monotonic() - started < 10
    assert_provider_stop(result, requests, "timed out")


def test_cross_origin_redirect_not_followed(binary):
    with ModelServer([completion()]) as other:
        handler = raw(b"HTTP/1.1 307 Temporary Redirect\r\nLocation: "
                      + other.base_url.encode() + b"/chat/completions\r\n"
                      b"Content-Length: 0\r\nConnection: close\r\n\r\n")
        result, requests = run_case(
            binary, [handler], extra=["--max-retries", "0"],
            auth='{ kind = "bearer_env", env = "MIN_AGENT_FIXTURE_BEARER" }',
            env_extra={"MIN_AGENT_FIXTURE_BEARER": "fixture-bearer"},
        )
        assert_provider_stop(result, requests, "HTTP status 307")
        assert requests[0]["headers"].get("authorization") == "Bearer fixture-bearer"
        assert other.requests == [], "redirect target received a request"


def test_event_stream_reply_rejected(binary):
    body = b'data: {"choices":[]}\n\n'
    handler = raw(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n"
                  b"Content-Length: %d\r\nConnection: close\r\n\r\n%s" % (len(body), body))
    result, requests = run_case(binary, [handler], extra=["--max-retries", "0"])
    assert_provider_stop(result, requests, "unexpected response content type")


def test_transient_503_is_retried(binary):
    handler = raw(b"HTTP/1.1 503 Service Unavailable\r\nRetry-After: 0\r\n"
                  b"Content-Length: 0\r\nConnection: close\r\n\r\n")
    result, requests = run_case(binary, [handler, completion()])
    assert_ok(result)
    assert len(requests) == 2


def test_round_budget_flag(binary):
    result, requests = run_case(
        binary, [completion(None, [call()]), completion()], extra=["--max-rounds", "1"])
    assert result.returncode == 2, (result.returncode, result.stderr)
    assert "model rounds" in result.stderr
    assert len(requests) == 1


def test_trace_is_metadata_only(binary):
    seen = {}

    def inspect(root):
        seen["lines"] = [json.loads(line) for line in (root / "trace.jsonl").read_text().splitlines()]
        seen["text"] = (root / "trace.jsonl").read_text()

    result, requests = run_case(
        binary, [completion(None, [call()]), completion()],
        extra=["--trace", "{root}/trace.jsonl"], inspect=inspect)
    assert_ok(result)
    lines = seen["lines"]
    assert [l["kind"] for l in lines] == [
        "run_start", "model_response", "tool_call", "model_response", "run_end"]
    assert [l["seq"] for l in lines] == list(range(len(lines)))
    assert len({l["run_id"] for l in lines}) == 1
    header = lines[0]["payload"]
    assert header["meta"]["profile"] == "fixture"
    assert header["meta"]["connection_fingerprint"]
    assert header["tools"] == ["list_files", "read_file", "search_text"]
    assert header["effects"] == ["read"]
    assert lines[2]["payload"]["path"] == "hello.txt" and lines[2]["payload"]["bytes"] > 0
    assert lines[-1]["payload"]["stop"]["kind"] == "completed"
    assert "UNIQUE_FIXTURE_CONTENT" not in seen["text"]


def responses_body(output):
    return {"id": "resp", "object": "response", "status": "completed", "output": output}


def test_responses_protocol(binary):
    result, requests = run_case(binary, [
        responses_body([
            {"type": "reasoning", "id": "rs_1", "summary": [], "encrypted_content": "opaque"},
            {"type": "function_call", "id": "fc_1", "call_id": "call_1", "name": "read_file",
             "arguments": '{"path":"hello.txt"}'},
        ]),
        responses_body([{"type": "message", "role": "assistant",
                         "content": [{"type": "output_text", "text": "fixture complete"}]}]),
    ], protocol="openai_responses")
    assert_ok(result)
    assert "fixture complete" in result.stdout
    assert requests[0]["path"] == "/v1/responses"
    assert requests[0]["body"]["store"] is False
    replay = requests[1]["body"]["input"]
    assert replay[1]["encrypted_content"] == "opaque"
    output = [i for i in replay if i.get("type") == "function_call_output"][0]
    assert output["call_id"] == "call_1" and "UNIQUE_FIXTURE_CONTENT" in output["output"]


def test_messages_protocol(binary):
    def message(stop, content):
        return {"id": "msg", "type": "message", "role": "assistant", "model": "fixture-model",
                "stop_reason": stop, "content": content}
    result, requests = run_case(binary, [
        message("tool_use", [
            {"type": "thinking", "thinking": "t", "signature": "sig"},
            {"type": "tool_use", "id": "toolu_1", "name": "read_file", "input": {"path": "hello.txt"}},
        ]),
        message("end_turn", [{"type": "text", "text": "fixture complete"}]),
    ], protocol="anthropic_messages",
        auth='{ kind = "header_env", header = "x-api-key", env = "MIN_AGENT_FIXTURE_KEY" }',
        env_extra={"MIN_AGENT_FIXTURE_KEY": "fixture-key"})
    assert_ok(result)
    assert requests[0]["path"] == "/v1/messages"
    headers = requests[0]["headers"]
    assert headers.get("x-api-key") == "fixture-key"
    assert headers.get("anthropic-version") == "2023-06-01"
    assert "authorization" not in headers
    history = requests[1]["body"]["messages"]
    assert history[1]["content"][0]["signature"] == "sig"
    result_block = history[2]["content"][0]
    assert result_block["tool_use_id"] == "toolu_1"
    assert "UNIQUE_FIXTURE_CONTENT" in result_block["content"]


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
