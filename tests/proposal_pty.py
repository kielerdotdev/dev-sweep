#!/usr/bin/env python3
"""Exercise cleanup plans in a real terminal with a local decision-model fixture.

Pass the Rust unit-test executable, and optionally a directory for HTML/text
captures. The ignored Rust fixture supplies the isolated scan configuration and
local provider. Exact approval removes only disposable fixture caches in one
scenario; the other scenarios never approve deletion or use a real API key.
"""
import fcntl
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import pty
import re
import select
import struct
import subprocess
import sys
import tempfile
import termios
import threading
import time

from terminal_screen import Screen


CONTENT = "PRIVATE FILE CONTENT MUST NOT BE SENT TO A DECISION MODEL"
FIXTURE_TEST = "tui::tests::proposal_terminal_fixture"


def make_fixture(root):
    caches = []
    for name in ["01-rebuild", "02-rebuild", "03-inspect"]:
        project = root / name
        cache = project / "node_modules"
        cache.mkdir(parents=True)
        (project / "package.json").write_text("{}")
        (cache / "generated.js").write_text(CONTENT)
        caches.append(cache)

    source = root / "repository"
    source.mkdir()

    def git(*args):
        subprocess.run(
            ["git", "-C", str(source), *args], check=True, capture_output=True
        )

    git("init", "--initial-branch=main")
    git("config", "user.name", "dev-sweep terminal fixture")
    git("config", "user.email", "fixture@example.invalid")
    (source / "package.json").write_text("{}")
    (source / ".gitignore").write_text("node_modules/\n")
    git("add", ".")
    git("commit", "-m", "fixture source")
    checkout = root / "recent-checkout"
    git("worktree", "add", "-b", "fixture-checkout", str(checkout))
    cache = checkout / "node_modules"
    cache.mkdir()
    (cache / "generated.js").write_text(CONTENT)
    caches.append(cache)

    old = time.time() - 90 * 86400
    for parent, _, files in os.walk(root, topdown=False):
        for name in files:
            os.utime(Path(parent) / name, (old, old))
        os.utime(parent, (old, old))
    # The checkout must be kept, while its ignored dependencies can be reviewed
    # as a separate generated artifact. Touching the checkout does not touch the
    # dependency directory or its payload.
    os.utime(checkout, None)
    return caches, checkout, source


class ModelServer(ThreadingHTTPServer):
    daemon_threads = True

    def __init__(self, failure):
        super().__init__(("127.0.0.1", 0), ModelHandler)
        self.failure = failure
        self.requests = []
        self.errors = []
        self.lock = threading.Lock()


class ModelHandler(BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def do_POST(self):
        try:
            body = self.rfile.read(int(self.headers["Content-Length"]))
            request = json.loads(body)
            assert self.path == "/decisions", self.path
            assert self.headers.get("Authorization") == "Bearer fixture-secret"
            assert b"fixture-secret" not in body, "key leaked into model prompt"
            assert CONTENT.encode() not in body, "file contents leaked into prompt"
            assert request["model"] == "cloudflare/clef", request
            assert "messages" not in request, "used the chat API contract"
            assert request["questions"]["review"]["type"] == "choice", request
            state = request["state"]
            assert state["local_checks_passed"] is True, state
            assert state["local_blocker"] is None, state
            with self.server.lock:
                self.server.requests.append(request)
            # Long enough to inspect the active progress screen, and staggered
            # enough to observe completed work before the last result arrives.
            delay = 0.6 if "02-rebuild" in state["resolved_cleanup_scope"] else 1.0
            time.sleep(delay)
            if self.server.failure:
                self.send_body(401, {"error": {"message": "invalid fixture credentials"}})
                return
            choice = (
                "investigate"
                if "03-inspect" in state["resolved_cleanup_scope"]
                else "regenerable"
            )
            distribution = {"regenerable": 0.05, "investigate": 0.05, "retain": 0.05}
            distribution[choice] = 0.9
            self.send_body(
                200,
                {
                    "model": "cloudflare/clef",
                    "answers": {
                        "review": {
                            "type": "choice",
                            "choice": choice,
                            "probabilities": distribution,
                            "confidence": 0.85,
                        }
                    },
                },
            )
        except Exception as error:
            with self.server.lock:
                self.server.errors.append(repr(error))
            self.send_body(500, {"error": "invalid fixture request"})

    def send_body(self, status, value):
        body = json.dumps(value).encode()
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Connection", "close")
        self.end_headers()
        self.wfile.write(body)


class Terminal:
    def __init__(self, binary, root, server, width, height):
        self.master, self.slave = pty.openpty()
        fcntl.ioctl(
            self.slave, termios.TIOCSWINSZ, struct.pack("HHHH", height, width, 0, 0)
        )
        self.original = termios.tcgetattr(self.slave)
        environment = os.environ.copy()
        for key in [
            "OPENROUTER_API_KEY",
            "OPENROUTER_MODEL",
            "CLOUDFLARE_API_TOKEN",
            "CLOUDFLARE_AUTH_TOKEN",
            "CLOUDFLARE_ACCOUNT_ID",
            "TYPESAFE_API_KEY",
            "HTTP_PROXY",
            "HTTPS_PROXY",
            "ALL_PROXY",
            "http_proxy",
            "https_proxy",
            "all_proxy",
            "NO_COLOR",
        ]:
            environment.pop(key, None)
        environment.update(
            TERM="xterm-256color",
            COLORTERM="truecolor",
            NO_PROXY="127.0.0.1,localhost",
            DEV_SWEEP_FIXTURE_ROOT=str(root),
            DEV_SWEEP_FIXTURE_URL=f"http://127.0.0.1:{server.server_port}/decisions",
        )
        self.process = subprocess.Popen(
            [str(binary), "--ignored", "--exact", FIXTURE_TEST, "--nocapture"],
            stdin=self.slave,
            stdout=self.slave,
            stderr=self.slave,
            cwd="/tmp",
            env=environment,
        )
        self.screen = Screen(width, height)
        self.output = bytearray()

    def pump(self, seconds=0.1):
        deadline = time.monotonic() + seconds
        while time.monotonic() < deadline:
            remaining = max(0, deadline - time.monotonic())
            if select.select([self.master], [], [], min(0.03, remaining))[0]:
                data = os.read(self.master, 65536)
                if not data:
                    break
                self.output.extend(data)
                self.screen.feed(data)

    def send(self, keys, seconds=0.1):
        os.write(self.master, keys)
        self.pump(seconds)

    def wait(self, predicate, description, timeout=12):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            self.pump()
            if predicate(self.screen.text):
                return
            if self.process.poll() is not None:
                break
        raise AssertionError(
            f"{description}; exit={self.process.poll()}\n{self.screen.text}"
        )

    def capture(self, directory, label):
        if directory:
            (directory / f"{label}.html").write_text(self.screen.html())
            (directory / f"{label}.txt").write_text(self.screen.text)

    def finish(self):
        self.send(b"q")
        self.process.wait(timeout=5)
        self.pump()
        assert self.process.returncode == 0, self.screen.text
        assert termios.tcgetattr(self.slave) == self.original, "terminal modes not restored"
        assert b"\x1b[?1049l" in self.output, "alternate screen not restored"

    def close(self):
        if self.process.poll() is None:
            self.process.kill()
            self.process.wait()
        os.close(self.master)
        os.close(self.slave)


def run(binary, capture_directory, width, height, failure):
    scenario = "credentials" if failure else "plan"
    label = f"{width}x{height}-{scenario}"
    with tempfile.TemporaryDirectory(prefix="dev-sweep-proposal-test-") as directory:
        root = Path(directory)
        caches, checkout, source = make_fixture(root)
        server = ModelServer(failure)
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        terminal = Terminal(binary, root, server, width, height)

        def preserved():
            assert all(path.is_dir() for path in caches), "plan review removed a cache"
            assert checkout.is_dir(), "plan review removed the active checkout"
            assert (source / "package.json").is_file(), "main source was removed"

        try:
            terminal.wait(lambda text: "READY" in text, "inventory did not become ready")
            terminal.capture(capture_directory, f"{label}-ready")
            terminal.send(b"p")
            terminal.wait(
                lambda text: "Build cleanup plan" in text,
                "p did not expose the consent and next action",
            )
            assert "Start AI review" in terminal.screen.text, terminal.screen.text
            assert not server.requests, "sent metadata before approval"
            terminal.capture(capture_directory, f"{label}-consent")
            terminal.send(b"\r", seconds=0.1)
            terminal.wait(
                lambda text: "AI REVIEW" in text,
                "starting review did not show active progress",
                timeout=0.5,
            )
            assert re.search(r"\b\d+\s*(?:/|of)\s*4\b", terminal.screen.text), (
                "review does not show candidate progress\n" + terminal.screen.text
            )
            terminal.capture(capture_directory, f"{label}-progress")
            preserved()

            if failure:
                terminal.wait(
                    lambda text: "HTTP 401" in text,
                    "credential failure was hidden instead of explaining the stalled plan",
                )
                assert "credentials" in terminal.screen.text.lower(), terminal.screen.text
                terminal.capture(capture_directory, f"{label}-error")
                terminal.send(b"\x1b")
                assert "0 selected" in terminal.screen.text, terminal.screen.text
            else:
                terminal.wait(
                    lambda text: "Review cleanup plan" in text,
                    "AI review did not open the cleanup plan",
                )
                terminal.capture(capture_directory, f"{label}-draft")
                preserved()
                assert len(server.requests) == 4, server.requests
                requested = {
                    Path(request["state"]["resolved_cleanup_scope"])
                    for request in server.requests
                }
                assert requested == set(caches), (
                    "did not review independent caches inside a retained worktree",
                    requested,
                )
                assert checkout not in requested, "proposed deleting a recent worktree"
                terminal.send(b" ")
                terminal.capture(capture_directory, f"{label}-exception")
                terminal.send(b"\x1b")
                assert "0 selected" in terminal.screen.text, (
                    "cancel adopted the draft\n" + terminal.screen.text
                )
                preserved()
                terminal.send(b"p")
                terminal.wait(
                    lambda text: "Review cleanup plan" in text,
                    "p did not reopen the completed plan",
                )
                assert len(server.requests) == 4, "reopening made another paid review"
                assert "DELETE 2" in terminal.screen.text, (
                    "Space did not remove a draft exception\n" + terminal.screen.text
                )
                terminal.capture(capture_directory, f"{label}-approval")
                preserved()
                terminal.send(b"\r")
                assert "Review cleanup plan" in terminal.screen.text, (
                    "blank Enter skipped the typed approval\n" + terminal.screen.text
                )
                preserved()
                terminal.send(b"yes\r")
                assert "Review cleanup plan" in terminal.screen.text, terminal.screen.text
                preserved()
                if width == 120:
                    terminal.send(b"\x7f\x7f\x7f")
                    terminal.send(b"DELETE 2\r")
                    terminal.wait(
                        lambda _: not caches[1].exists() and not caches[3].exists(),
                        "exact approval did not remove the two checked fixture caches",
                    )
                    assert caches[0].is_dir(), "removed a deselected cache"
                    assert caches[2].is_dir(), "removed an investigate-only cache"
                    assert checkout.is_dir(), "removed a retained worktree"
                    assert (source / "package.json").is_file(), "removed main source"
                    subprocess.run(
                        ["git", "-C", str(source), "show-ref", "--verify", "refs/heads/fixture-checkout"],
                        check=True,
                        capture_output=True,
                    )
                    terminal.capture(capture_directory, f"{label}-completed")
                else:
                    terminal.send(b"\x1b")

            assert not server.errors, server.errors
            for request in server.requests:
                scope = Path(request["state"]["resolved_cleanup_scope"])
                assert scope in caches, "sent retained or external scope to model"
            assert "fixture-secret" not in terminal.screen.text, "key leaked into UI"
            if failure or width != 120:
                preserved()
            terminal.finish()
            print(
                f"PTY proposal {label} passed: visible progress, local model requests, "
                + (
                    "credential error, no selection or deletion, terminal restoration."
                    if failure
                    else "draft exceptions, cancellation, saved plan, typed approval, "
                    "retained worktree cache fallback, "
                    + (
                        "only checked fixture caches removed, "
                        if width == 120
                        else "no deletion, "
                    )
                    + "terminal restoration."
                )
            )
        finally:
            terminal.close()
            server.shutdown()
            server.server_close()
            thread.join(timeout=2)


if __name__ == "__main__":
    if len(sys.argv) < 2:
        build = subprocess.run(["cargo", "test", "--locked", "--no-run", "--message-format=json"], cwd=Path(__file__).resolve().parent.parent, capture_output=True, text=True, check=True)
        artifacts = [json.loads(line) for line in build.stdout.splitlines() if line.startswith("{")]
        candidates = [item["executable"] for item in artifacts if item.get("reason") == "compiler-artifact" and item.get("target", {}).get("name") == "dev-sweep" and item.get("profile", {}).get("test") and item.get("executable")]
        if len(candidates) != 1:
            raise SystemExit("Could not find the compiled Rust unit-test fixture")
        binary = Path(candidates[0])
    else:
        binary = Path(sys.argv[1]).resolve()
    captures = Path(sys.argv[2]).resolve() if len(sys.argv) > 2 else None
    if captures:
        captures.mkdir(parents=True, exist_ok=True)
    for width, height in [(120, 36), (80, 30)]:
        for failure in [False, True]:
            run(binary, captures, width, height, failure)
