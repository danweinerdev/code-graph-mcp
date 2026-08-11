#!/usr/bin/env python3
"""Measure warm daemon attach against cold in-process indexing.

This is a manual benchmark, not a test gate. It uses only the Python standard
library and preserves an existing project cache. Typical use:

    make build
    python3 scripts/bench-daemon-attach.py --binary target/release/code-graph-mcp \
      --corpus external/ripgrep --repetitions 5 --json

Run it separately for ``external/ripgrep`` and ``external/abseil-cpp``. The
JSON output contains every sample and the median; no plan or result note is
written by this script.
"""

import argparse
import json
import os
import selectors
import shutil
import subprocess
import sys
import tempfile
import time
from pathlib import Path
from statistics import median


class McpClient:
    """Small synchronous JSON-RPC client for the newline MCP transport."""

    def __init__(self, command: list[str], cwd: Path, request_deadline_s: float):
        self.proc = subprocess.Popen(
            command,
            cwd=cwd,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        self.next_id = 0
        self.request_deadline_s = request_deadline_s
        self.selector = selectors.DefaultSelector()
        assert self.proc.stdout
        self.selector.register(self.proc.stdout, selectors.EVENT_READ)

    def close(self) -> str:
        self.selector.close()
        if self.proc.stdin and not self.proc.stdin.closed:
            self.proc.stdin.close()
        try:
            self.proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            self.proc.kill()
            self.proc.wait()
        return (self.proc.stderr.read() if self.proc.stderr else b"").decode(
            errors="replace"
        )

    def request(self, method: str, params: dict) -> dict:
        self.next_id += 1
        message = {"jsonrpc": "2.0", "id": self.next_id, "method": method, "params": params}
        assert self.proc.stdin and self.proc.stdout
        self.proc.stdin.write((json.dumps(message) + "\n").encode())
        self.proc.stdin.flush()
        deadline = time.monotonic() + self.request_deadline_s
        while True:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise TimeoutError(
                    f"RPC {method} exceeded {self.request_deadline_s:g}-second deadline"
                )
            if not self.selector.select(remaining):
                raise TimeoutError(
                    f"RPC {method} exceeded {self.request_deadline_s:g}-second deadline"
                )
            line = self.proc.stdout.readline()
            if not line:
                raise RuntimeError("server closed before response")
            response = json.loads(line)
            if response.get("id") == self.next_id:
                return response

    def notify(self, method: str, params: dict) -> None:
        assert self.proc.stdin
        self.proc.stdin.write(
            (json.dumps({"jsonrpc": "2.0", "method": method, "params": params}) + "\n").encode()
        )
        self.proc.stdin.flush()

    def initialize(self) -> None:
        response = self.request(
            "initialize",
            {
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": {"name": "bench-daemon-attach", "version": "1"},
            },
        )
        if "result" not in response:
            raise RuntimeError(f"initialize failed: {response}")
        self.notify("notifications/initialized", {})

    def tool(self, name: str, arguments: dict) -> dict:
        response = self.request("tools/call", {"name": name, "arguments": arguments})
        if "result" not in response or response["result"].get("isError"):
            raise RuntimeError(f"{name} failed: {response}")
        return response


def indexed(client: McpClient, corpus: Path, force: bool) -> dict:
    response = client.tool("analyze_codebase", {"path": str(corpus), "force": force})
    try:
        result = json.loads(response["result"]["content"][0]["text"])
        counts = {name: result[name] for name in ("files", "symbols", "edges")}
    except (IndexError, KeyError, TypeError, json.JSONDecodeError) as error:
        raise RuntimeError(f"analyze_codebase returned invalid result: {response}") from error
    if not all(isinstance(value, int) for value in counts.values()):
        raise RuntimeError(f"analyze_codebase returned non-integer counts: {result}")
    return counts


SHIPPED_EXTENSIONS = {
    ".cpp", ".cc", ".cxx", ".c", ".h", ".hpp", ".hxx", ".rs", ".go",
    ".py", ".pyi", ".cs", ".java",
}


def source_candidates(corpus: Path) -> list[Path]:
    """Return stable shipped-language candidates, excluding daemon state."""
    return sorted(
        (
            path
            for path in corpus.rglob("*")
            if path.is_file()
            and ".code-graph" not in path.parts
            and path.suffix.lower() in SHIPPED_EXTENSIONS
        ),
        key=lambda path: path.relative_to(corpus).as_posix(),
    )


def first_query(client: McpClient, candidates: list[Path]) -> Path:
    """Find an indexed source with the first successful graph query."""
    for path in candidates:
        response = client.request(
            "tools/call",
            {
                "name": "get_file_symbols",
                "arguments": {"file": str(path), "brief": True, "count_only": True},
            },
        )
        if "result" in response and not response["result"].get("isError"):
            return path
    raise RuntimeError("no shipped-language source file was indexed")


def wait_for_metadata(runtime: Path, timeout_s: float = 10.0) -> dict:
    metadata_path = runtime / "daemon.json"
    deadline = time.monotonic() + timeout_s
    while time.monotonic() < deadline:
        try:
            return json.loads(metadata_path.read_text())
        except (FileNotFoundError, json.JSONDecodeError):
            time.sleep(0.02)
    raise RuntimeError(f"daemon did not publish {metadata_path}")


def stop_daemon(runtime: Path) -> bool:
    """Request shutdown only from metadata proven to own the live lock."""
    metadata_path = runtime / "daemon.json"
    lock_path = runtime / "daemon.lock"
    request_path = runtime / "shutdown.request"
    try:
        metadata = json.loads(metadata_path.read_text())
        owner = metadata["owner"]
        if owner != json.loads(lock_path.read_text()):
            raise RuntimeError("daemon metadata owner does not match daemon.lock")
    except (FileNotFoundError, json.JSONDecodeError, KeyError) as error:
        raise RuntimeError("cannot prove benchmark daemon ownership") from error
    try:
        descriptor = os.open(
            request_path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600
        )
        with os.fdopen(descriptor, "w") as request:
            os.fchmod(request.fileno(), 0o600)
            json.dump(owner, request, separators=(",", ":"))
            request.flush()
            os.fsync(request.fileno())
    except FileExistsError as error:
        raise RuntimeError("refusing to replace existing shutdown.request") from error
    deadline = time.monotonic() + 10.0
    while time.monotonic() < deadline:
        if not lock_path.exists():
            if metadata_path.exists():
                raise RuntimeError("daemon released lock without owned metadata cleanup")
            return True
        time.sleep(0.02)
    raise RuntimeError("benchmark daemon did not cleanly exit")


def measure_warm(
    binary: Path, corpus: Path, source: Path, repetitions: int, request_deadline_s: float
) -> list[float]:
    samples = []
    for _ in range(repetitions):
        started = time.monotonic()
        client = McpClient([str(binary)], corpus, request_deadline_s)
        try:
            client.initialize()
            first_query(client, [source])
            samples.append(time.monotonic() - started)
        finally:
            client.close()
    return samples


def measure_cold(
    binary: Path, corpus: Path, source: Path, repetitions: int, request_deadline_s: float
) -> tuple[list[float], list[dict]]:
    samples = []
    counts = []
    for _ in range(repetitions):
        started = time.monotonic()
        client = McpClient([str(binary), "--no-daemon"], corpus, request_deadline_s)
        try:
            client.initialize()
            counts.append(indexed(client, corpus, force=True))
            first_query(client, [source])
            samples.append(time.monotonic() - started)
        finally:
            client.close()
    return samples, counts


def restore_benchmark_state(
    runtime: Path, cache: Path, cache_backup: Path, had_cache: bool,
    config: Path, created_config: bool,
) -> Exception | None:
    """Attempt every independent cleanup step and retain the first failure."""
    first_error = None

    def attempt(action) -> None:
        nonlocal first_error
        try:
            action()
        except Exception as error:
            if first_error is None:
                first_error = error

    if runtime.exists():
        def stop_runtime() -> None:
            if not stop_daemon(runtime):
                raise RuntimeError("benchmark daemon did not complete owned shutdown")
            runtime.rmdir()
        attempt(stop_runtime)
    if had_cache:
        attempt(lambda: shutil.copy2(cache_backup, cache))
    else:
        attempt(lambda: cache.unlink(missing_ok=True))
    if created_config:
        attempt(lambda: config.unlink(missing_ok=True))
    return first_error


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", required=True, type=Path, help="built code-graph-mcp binary")
    parser.add_argument("--corpus", required=True, type=Path, help="corpus root, e.g. external/ripgrep")
    parser.add_argument("--repetitions", type=int, default=5, help="samples per mode (default: 5)")
    parser.add_argument(
        "--request-deadline-seconds", type=float, default=900,
        help="maximum time for each RPC response (default: 900)",
    )
    parser.add_argument("--json", action="store_true", help="emit one JSON result object")
    args = parser.parse_args()
    binary = args.binary.resolve()
    corpus = args.corpus.resolve()
    if not binary.is_file() or not os.access(binary, os.X_OK):
        parser.error(f"--binary is not executable: {binary}")
    if not corpus.is_dir():
        parser.error(f"--corpus is not a directory: {corpus}")
    if args.repetitions < 1:
        parser.error("--repetitions must be at least 1")
    if args.request_deadline_seconds <= 0:
        parser.error("--request-deadline-seconds must be positive")

    runtime = corpus / ".code-graph"
    cache = corpus / ".code-graph-cache.db"
    config = corpus / ".code-graph.toml"
    if runtime.exists():
        parser.error(f"refusing to disturb existing daemon runtime: {runtime}")
    for path in (cache, config):
        if path.is_symlink():
            parser.error(f"refusing to replace symlinked benchmark state: {path}")

    with tempfile.TemporaryDirectory(prefix="code-graph-daemon-bench-") as temporary:
        cache_backup = Path(temporary) / "cache.db"
        had_cache = cache.exists()
        had_config = config.exists()
        if had_cache:
            shutil.copy2(cache, cache_backup)
        created_config = False
        try:
            # An existing config controls indexing and must remain byte-for-byte
            # untouched. When absent, this empty config only establishes the
            # corpus as a project root; daemon defaults provide the idle policy.
            if not had_config:
                config.write_text("# benchmark project boundary\n")
                created_config = True
            # Warm the daemon and index once before sampling attachment.
            bootstrap = McpClient([str(binary)], corpus, args.request_deadline_seconds)
            try:
                bootstrap.initialize()
                bootstrap_counts = indexed(bootstrap, corpus, force=True)
                source = first_query(bootstrap, source_candidates(corpus))
                wait_for_metadata(runtime)
                # Keep this admitted client attached while measuring warm
                # attaches, including corpora configured with a short idle
                # timeout.
                warm = measure_warm(
                    binary, corpus, source, args.repetitions, args.request_deadline_seconds
                )
            finally:
                bootstrap.close()
            if not stop_daemon(runtime):
                raise RuntimeError("benchmark daemon did not complete owned shutdown")
            runtime.rmdir()
            cold, cold_counts = measure_cold(
                binary, corpus, source, args.repetitions, args.request_deadline_seconds
            )
            for sample_counts in cold_counts:
                if sample_counts != bootstrap_counts:
                    raise RuntimeError(
                        "cold force analyze indexed different counts: "
                        f"bootstrap={bootstrap_counts}, cold={sample_counts}"
                    )
        finally:
            cleanup_error = restore_benchmark_state(
                runtime, cache, cache_backup, had_cache, config, created_config
            )
            if cleanup_error is not None:
                raise cleanup_error

    result = {
        "binary": str(binary),
        "corpus": str(corpus),
        "source_file": str(source),
        "file_count": sum(1 for path in corpus.rglob("*") if path.is_file()),
        "indexed_counts": bootstrap_counts,
        "repetitions": args.repetitions,
        "warm_attach_seconds": {"samples": warm, "median": median(warm)},
        "cold_no_daemon_seconds": {"samples": cold, "median": median(cold)},
    }
    if args.json:
        print(json.dumps(result, sort_keys=True))
    else:
        print(json.dumps(result, indent=2, sort_keys=True))
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except Exception as error:
        print(f"bench-daemon-attach: {error}", file=sys.stderr)
        raise SystemExit(1)
