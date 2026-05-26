#!/usr/bin/env python3
"""Manual smoke: end-to-end stdio drive of analyze_codebase_async.

What this exists for
====================
Unit and integration tests cover the slot protocol from the Rust side —
they instantiate `CodeGraphServer`, call handler entry points directly,
and assert in-process state. They do NOT exercise:

  * the rmcp stdio framing (`initialize` handshake, `tools/call`
    envelope, JSON-RPC ids on the wire),
  * the `analyze_codebase_async` tool being registered on the router
    under that exact name with that exact argument schema,
  * the response text being a parseable JSON body matching the agent's
    expected shape,
  * the per-phase `eprintln!` operator-visibility logs still firing
    under the async code path (Decision 8 / Task 1.2 preservation
    contract — agents AND human operators read these out of `stderr`),
  * the kickoff actually returning in well under the agent's tool-call
    timeout on a real-sized codebase.

This driver does all six, against the release binary built by
`make build`. It is the canonical end-of-Phase-2 manual-smoke gate
referenced in `.plans/Plans/AnalyzeCodebaseAsync/02-Testing.md` task
2.5, kept in `scripts/` so it can be re-run on every future change
that touches the async kickoff path or the get_status wire shape.

Usage
=====
    make build
    python3 scripts/smoke-analyze-async.py [TARGET]

TARGET is the directory to index. Defaults to `external/ripgrep`
(initialized via `make submodules`) when present; falls back to the
repository root otherwise. Pass an explicit path to point at a different
corpus.

The script exits 0 on success, non-zero with a diagnostic line on the
first failed assertion. It is NOT wired into `make test` — invoking the
release binary against a real codebase is not free, and the unit +
integration tests guard the same invariants on every commit. This is a
smoke test, run by hand at phase boundaries.

The driver writes a small JSONL transcript to stderr ("STEP N ..." lines)
plus the captured stderr of the server, so a successful run is its own
audit trail when piped to a file.
"""

import json
import os
import re
import subprocess
import sys
import threading
import time
from pathlib import Path


# ---------------------------------------------------------------------------
# Configuration
# ---------------------------------------------------------------------------

REPO_ROOT = Path(__file__).resolve().parent.parent
RELEASE_BIN = REPO_ROOT / "target" / "release" / "code-graph-mcp"

POLL_INTERVAL_S = 0.05
KICKOFF_BUDGET_S = 1.0

# The five per-phase eprintln! prefixes the analyze worker emits, in order
# (see crates/code-graph-tools/src/handlers/analyze.rs). Each is asserted
# present in the captured stderr; if any goes missing the operator-visibility
# contract has regressed.
PHASE_PREFIXES = [
    "[code-graph] phase: loading cache from",
    "[code-graph] phase: discovering + parsing under",
    "[code-graph] phase: resolving edges",
    "[code-graph] phase: merging",
    "[code-graph] phase: saving cache to",
]


# ---------------------------------------------------------------------------
# Stdio JSON-RPC plumbing
# ---------------------------------------------------------------------------


class McpClient:
    """Minimal blocking JSON-RPC 2.0 client over a child's stdio."""

    def __init__(self, proc: subprocess.Popen):
        self.proc = proc
        self._next_id = 0
        # Drain stderr in the background so the child's pipe buffer can't
        # fill and block the worker once it starts printing per-phase logs.
        self.stderr_chunks: list[str] = []
        self._stderr_thread = threading.Thread(
            target=self._drain_stderr, daemon=True
        )
        self._stderr_thread.start()

    def _drain_stderr(self) -> None:
        assert self.proc.stderr is not None
        for raw in iter(self.proc.stderr.readline, b""):
            self.stderr_chunks.append(raw.decode(errors="replace"))

    def _send(self, msg: dict) -> None:
        assert self.proc.stdin is not None
        self.proc.stdin.write((json.dumps(msg) + "\n").encode())
        self.proc.stdin.flush()

    def _recv(self) -> dict:
        assert self.proc.stdout is not None
        line = self.proc.stdout.readline()
        if not line:
            raise RuntimeError(
                "server closed stdout before sending a response; "
                "stderr tail: " + "".join(self.stderr_chunks)[-2000:]
            )
        return json.loads(line.decode())

    def request(self, method: str, params: dict | None = None) -> dict:
        self._next_id += 1
        req_id = self._next_id
        self._send(
            {
                "jsonrpc": "2.0",
                "id": req_id,
                "method": method,
                "params": params or {},
            }
        )
        while True:
            msg = self._recv()
            # Ignore unrelated server-initiated notifications (no "id" field)
            # and any out-of-band progress notifications. We only block on
            # the response matching our request id.
            if msg.get("id") == req_id:
                return msg

    def notify(self, method: str, params: dict | None = None) -> None:
        self._send({"jsonrpc": "2.0", "method": method, "params": params or {}})

    def call_tool(self, name: str, arguments: dict) -> dict:
        return self.request(
            "tools/call", {"name": name, "arguments": arguments}
        )

    def stderr_text(self) -> str:
        return "".join(self.stderr_chunks)


def parse_tool_text(resp: dict) -> dict:
    """Extract and JSON-decode the text payload of a CallToolResult."""
    if "error" in resp:
        raise RuntimeError(f"tool error: {resp['error']}")
    content = resp.get("result", {}).get("content")
    if not content:
        raise RuntimeError(f"no content in response: {resp}")
    text = content[0].get("text")
    if not text:
        raise RuntimeError(f"no text in content[0]: {content[0]}")
    return json.loads(text)


# ---------------------------------------------------------------------------
# Smoke steps
# ---------------------------------------------------------------------------


def step_initialize(client: McpClient) -> None:
    resp = client.request(
        "initialize",
        {
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": {"name": "smoke-analyze-async", "version": "1"},
        },
    )
    if "result" not in resp:
        raise RuntimeError(f"initialize failed: {resp}")
    client.notify("notifications/initialized")
    print("STEP 1 initialize: ok", file=sys.stderr)


def step_kickoff_async(client: McpClient, target: Path) -> str:
    t0 = time.monotonic()
    resp = client.call_tool(
        "analyze_codebase_async", {"path": str(target), "force": False}
    )
    elapsed = time.monotonic() - t0
    body = parse_tool_text(resp)
    print(
        f"STEP 2 kickoff: elapsed_s={elapsed:.3f} body={json.dumps(body)}",
        file=sys.stderr,
    )
    if elapsed >= KICKOFF_BUDGET_S:
        raise AssertionError(
            f"kickoff took {elapsed:.3f}s, exceeds {KICKOFF_BUDGET_S}s budget"
        )
    if body.get("status") != "running":
        raise AssertionError(f"expected status=running, got {body}")
    if body.get("existing") is not False:
        raise AssertionError(f"expected existing=false, got {body}")
    job_id = body.get("job_id")
    if not job_id:
        raise AssertionError(f"empty/missing job_id: {body}")
    return job_id


def step_poll_until_terminal(
    client: McpClient, job_id: str, budget_s: float
) -> dict:
    deadline = time.monotonic() + budget_s
    saw_progress_nonzero = False
    saw_progress_message = False
    poll_count = 0
    samples: list[tuple[str, int, str | None]] = []
    while time.monotonic() < deadline:
        poll_count += 1
        body = parse_tool_text(client.call_tool("get_status", {}))
        job = body.get("analyze_job")
        if job is None:
            raise AssertionError(f"analyze_job is null mid-run: {body}")
        if job.get("job_id") != job_id:
            raise AssertionError(
                f"job_id changed mid-run: expected {job_id}, got {job}"
            )
        status = job.get("status")
        progress = int(job.get("progress") or 0)
        message = job.get("progress_message")
        samples.append((status, progress, message))
        if status == "running":
            if progress > 0:
                saw_progress_nonzero = True
            if message:
                saw_progress_message = True
        if status in ("completed", "failed"):
            terminal_progress = progress
            terminal_message = message
            print(
                f"STEP 3 polls={poll_count} mid_poll_progress_nonzero="
                f"{saw_progress_nonzero} mid_poll_message_seen="
                f"{saw_progress_message} terminal_progress="
                f"{terminal_progress} terminal_message_len="
                f"{len(terminal_message or '')}",
                file=sys.stderr,
            )
            print(
                "STEP 3 samples_head=" + json.dumps(samples[:5]),
                file=sys.stderr,
            )
            print(
                "STEP 3 samples_tail=" + json.dumps(samples[-3:]),
                file=sys.stderr,
            )
            if status != "completed":
                raise AssertionError(
                    f"job ended with status={status}: error={job.get('error')}"
                )
            # Progress-channel evidence: ideally we land at least one
            # mid-run poll with progress>0 and a non-empty message. On a
            # very fast corpus (release-mode ripgrep parses in <1s) the
            # 50ms cadence can still miss the in-progress window entirely;
            # in that case the terminal sample MUST carry a populated
            # progress_message — that proves the JobAwareProgressSink
            # wired through and the worker advanced the atomic at least
            # once before flipping to Completed. Both unit and integration
            # tests already pin the strict "progress>0 mid-run" invariant
            # deterministically via SLEEP_PER_PARSE_MS; this smoke just
            # proves the wire protocol carries it.
            evidence_ok = (
                (saw_progress_nonzero and saw_progress_message)
                or bool(terminal_message)
            )
            if not evidence_ok:
                raise AssertionError(
                    "no progress-channel evidence: never saw progress>0 + "
                    "non-empty message mid-run, AND terminal progress_message "
                    f"is empty. samples={samples}"
                )
            return job
        time.sleep(POLL_INTERVAL_S)
    raise AssertionError(
        f"polling timed out after {budget_s}s; last samples={samples[-5:]}"
    )


def step_get_file_symbols(client: McpClient, target: Path) -> None:
    candidate: Path | None = None
    for ext in (".rs", ".cpp", ".cc", ".c", ".go", ".py", ".cs", ".java"):
        for p in target.rglob(f"*{ext}"):
            parts = set(p.parts)
            if "target" in parts or ".git" in parts:
                continue
            candidate = p
            break
        if candidate is not None:
            break
    if candidate is None:
        raise AssertionError(f"no indexable source file under {target}")

    body = parse_tool_text(
        client.call_tool("get_file_symbols", {"file": str(candidate), "limit": 10})
    )
    results = body.get("results")
    total = body.get("total")
    print(
        f"STEP 4 file={candidate} total={total} results_len="
        f"{len(results) if results else 0}",
        file=sys.stderr,
    )
    if not results:
        raise AssertionError(f"no symbols extracted for {candidate}: {body}")
    # SymbolResult on the wire serializes the id field as "id" (not
    # "symbol_id" — that's the input/sort-key spelling elsewhere); see
    # crates/code-graph-tools/src/handlers/mod.rs::SymbolResult.
    sample_ids = [r.get("id") for r in results[:3]]
    print(f"STEP 4 sample_ids={sample_ids}", file=sys.stderr)


def step_verify_stderr_phases(client: McpClient) -> None:
    text = client.stderr_text()
    missing: list[str] = []
    for prefix in PHASE_PREFIXES:
        if prefix not in text:
            missing.append(prefix)
    print("STEP 5 stderr_tail:", file=sys.stderr)
    sys.stderr.write(text[-2000:])
    if not text.endswith("\n"):
        sys.stderr.write("\n")
    if missing:
        raise AssertionError(
            "missing per-phase eprintln prefix(es) in stderr: " + repr(missing)
        )
    print(f"STEP 5 phase_prefixes_present={len(PHASE_PREFIXES)}", file=sys.stderr)


# ---------------------------------------------------------------------------
# Entry point
# ---------------------------------------------------------------------------


def pick_target(argv: list[str]) -> tuple[Path, float]:
    """Resolve the target directory and a poll budget for it."""
    if len(argv) > 1:
        return Path(argv[1]).resolve(), 600.0
    ripgrep = REPO_ROOT / "external" / "ripgrep"
    if ripgrep.exists() and any(ripgrep.iterdir()):
        # ripgrep is the canonical Rust dogfood corpus; ~600 files indexes
        # comfortably under a minute in release mode, but we leave headroom
        # for the first cache build.
        return ripgrep, 300.0
    return REPO_ROOT, 120.0


def main(argv: list[str]) -> int:
    if not RELEASE_BIN.exists():
        print(
            f"release binary missing: {RELEASE_BIN}\n"
            "build first: make build",
            file=sys.stderr,
        )
        return 2

    target, poll_budget_s = pick_target(argv)
    print(f"smoke target: {target} (poll_budget_s={poll_budget_s})", file=sys.stderr)

    # Drop any cached graph for the chosen target so we observe a fresh
    # indexing run — otherwise a cached corpus returns in ~50ms with no
    # mid-run progress sample, falsifying the polling-progress assertion.
    cache = target / ".code-graph-cache.db"
    if cache.exists():
        cache.unlink()

    proc = subprocess.Popen(
        [str(RELEASE_BIN)],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        cwd=str(target),
    )
    client = McpClient(proc)
    try:
        step_initialize(client)
        job_id = step_kickoff_async(client, target)
        step_poll_until_terminal(client, job_id, poll_budget_s)
        step_get_file_symbols(client, target)
        # stderr verification runs LAST so the drainer has had time to
        # capture every per-phase emit.
        time.sleep(0.2)
        step_verify_stderr_phases(client)
    except Exception as exc:
        print(f"SMOKE FAIL: {exc}", file=sys.stderr)
        sys.stderr.write(client.stderr_text()[-3000:])
        return 1
    finally:
        try:
            proc.terminate()
            proc.wait(timeout=5)
        except Exception:
            proc.kill()

    print("SMOKE OK", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
