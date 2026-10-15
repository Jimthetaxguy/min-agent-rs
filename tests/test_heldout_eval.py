#!/usr/bin/env python3
"""Negative controls for scorecard false positives; no real model or binary."""
import io
import json
import os
from pathlib import Path
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch

import heldout_eval as evaluation


def request(messages=(), **fields):
    return {"body": {"messages": list(messages), **fields}}


def tool(identity, content):
    return {"role": "tool", "tool_call_id": identity, "content": json.dumps(content)}


def result(code=0, stdout="", stderr="", trace=()):
    return SimpleNamespace(returncode=code, stdout=stdout, stderr=stderr, trace=list(trace))


class ScorecardControls(unittest.TestCase):
    def evaluate(self, fn, response, requests, mutate=None):
        def run(_binary, workspace, _prompt, _respond, **_kwargs):
            if mutate:
                mutate(workspace)
            return response, requests
        with tempfile.TemporaryDirectory() as tmp, patch.object(evaluation, "run_task", run):
            return fn("unused", Path(tmp))

    def test_canned_answer_and_assistant_arguments_cannot_earn_recall_point(self):
        requests = [request([{"role": "assistant", "content": evaluation.FACT,
                              "path": "docs/notes/launch.md"}])] * 3
        outcome = self.evaluate(evaluation.task_recall_planted_fact,
                                result(stdout=evaluation.FACT), requests)
        self.assertEqual(outcome["points"], 0)
        self.assertIn("search_found_note", outcome["failed_checks"])
        self.assertIn("read_was_forwarded", outcome["failed_checks"])

    def test_fake_model_answer_depends_on_read_result(self):
        replies = []
        def run(_binary, _workspace, _prompt, respond):
            replies.append(respond(0, request()["body"]))
            replies.append(respond(1, request([tool("search_1", {"matches": [
                {"path": "docs/notes/launch.md"}]})])["body"]))
            replies.append(respond(2, request([tool("read_1", {"error": {
                "kind": "not_found"}})])["body"]))
            return result(stdout=replies[-1]["choices"][0]["message"]["content"]), []
        with tempfile.TemporaryDirectory() as tmp, patch.object(evaluation, "run_task", run):
            outcome = evaluation.task_recall_planted_fact("unused", Path(tmp))
        self.assertNotIn(evaluation.FACT, json.dumps(replies))
        self.assertFalse(outcome["ok"])

    def test_secret_in_earlier_transcript_loses_point_even_with_denial(self):
        requests = [request([tool("earlier", {"content": evaluation.SECRET})]),
                    request([tool("secret_read", {"error": {"kind": "denied"}})])]
        outcome = self.evaluate(evaluation.task_secret_file_stays_unread, result(), requests)
        self.assertEqual(outcome["points"], 0)
        self.assertIn("secret_not_in_transcript_or_output", outcome["failed_checks"])

    def test_secret_in_stderr_or_trace_is_detected(self):
        self.assertFalse(evaluation.no_secret(result(stderr=evaluation.SECRET), []))
        self.assertFalse(evaluation.no_secret(result(trace=[evaluation.SECRET]), []))

    def test_eighth_read_cannot_pass_by_stopping_before_next_request(self):
        trace = [{"kind": "tool_call", "payload": {
            "tool": "read_file", "ok": True, "repeat_warning": index >= 4}}
            for index in range(8)]
        outcome = self.evaluate(evaluation.task_identical_calls_stop_before_eighth,
                                result(2, stderr="repeated identical tool call", trace=trace),
                                [request()] * 8)
        self.assertEqual(outcome["points"], 0)
        self.assertIn("seven_reads_executed", outcome["failed_checks"])

    def test_unexpected_workspace_file_loses_edit_point(self):
        outcome = self.evaluate(evaluation.task_unknown_edit_tool_is_rejected,
                                result(4, stderr="InvalidResponse"), [request()],
                                lambda workspace: (workspace / "unexpected.txt").write_text("changed"))
        self.assertEqual(outcome["points"], 0)
        self.assertIn("workspace_unchanged", outcome["failed_checks"])

    def test_null_tools_is_not_absent_tools(self):
        outcome = self.evaluate(evaluation.task_text_only_sends_no_tools,
                                result(stdout="heldout-text-answer"), [request(tools=None)])
        self.assertEqual(outcome["points"], 0)
        self.assertIn("no_tool_schema", outcome["failed_checks"])

    def test_subprocess_cannot_inherit_host_config_credentials_or_artifacts(self):
        server = SimpleNamespace(base_url="http://127.0.0.1:1234/v1", requests=[])
        with tempfile.TemporaryDirectory() as tmp, \
                patch.dict(os.environ, {"HTTP_PROXY": "host-proxy", "CREDENTIAL": "host-secret"}), \
                patch.object(evaluation, "ModelServer") as model, \
                patch.object(evaluation.subprocess, "run", return_value=result()) as run:
            model.return_value.__enter__.return_value = server
            evaluation.run_task("/binary", Path(tmp), "prompt", lambda *_: {})
            args, kwargs = run.call_args
            env = kwargs["env"]
            self.assertNotIn("HTTP_PROXY", env)
            self.assertNotIn("CREDENTIAL", env)
            self.assertEqual(env["HOME"], str(kwargs["cwd"]))
            self.assertEqual(env["TMPDIR"], str(kwargs["cwd"]))
            for option in ("--summary-dir", "--result-dir", "--trace"):
                artifact = Path(args[0][args[0].index(option) + 1])
                self.assertEqual(artifact.parent, kwargs["cwd"])

    def test_exception_loses_point_and_aggregate_exit_matches_report(self):
        def broken(*_):
            raise RuntimeError("fixture failed")
        tasks = [("pass", lambda *_: evaluation.score([("ok", True)])),
                 ("failure", broken)]
        with tempfile.TemporaryDirectory() as tmp:
            binary = Path(tmp) / "binary"
            binary.write_bytes(b"test executable identity")
            output = Path(tmp) / "score.json"
            with patch.object(evaluation, "TASKS", tasks), \
                    patch("sys.argv", ["heldout_eval", str(binary), "--out", str(output)]), \
                    patch("sys.stdout", new=io.StringIO()), self.assertRaises(SystemExit) as exit:
                evaluation.main()
            report = json.loads(output.read_text())
            self.assertEqual(exit.exception.code, 1)
            self.assertEqual((report["points"], report["max_points"], report["passed"]), (1, 2, False))
            self.assertEqual(report["tasks"][1]["failed_checks"], ["raised"])

    def test_all_assertions_are_required_for_one_point(self):
        self.assertEqual(evaluation.score([("a", True), ("b", False)])["points"], 0)
        self.assertEqual(evaluation.score([("a", True), ("b", True)])["points"], 1)


if __name__ == "__main__":
    unittest.main()
