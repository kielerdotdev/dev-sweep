#!/usr/bin/env python3
"""Exercise real terminal approval for separate caches and bundled worktrees."""
import fcntl
import os
from pathlib import Path
import pty
import select
import struct
import subprocess
import sys
import tempfile
import termios
import time
from terminal_screen import Screen

binary = Path(sys.argv[1] if len(sys.argv) > 1 else "target/debug/dev-sweep").resolve()
for scenario in ["cache", "worktree"]:
    with tempfile.TemporaryDirectory(prefix="dev-sweep-tui-test-") as directory:
        root = Path(directory)
        project = root / "project"
        project.mkdir()
        (project / "package.json").write_text("{}")
        source_project = project
        if scenario == "worktree":
            def git(*args):
                subprocess.run(["git", "-C", str(source_project), *args], check=True, capture_output=True)
            git("init", "--initial-branch=main")
            git("config", "user.name", "dev-sweep fixture")
            git("config", "user.email", "fixture@example.invalid")
            (project / ".gitignore").write_text("node_modules/\ntarget/\n")
            (project / "Cargo.toml").write_text('[package]\nname = "fixture"\nversion = "0.1.0"\n')
            git("add", ".")
            git("commit", "-m", "fixture")
            project = root / "checkout"
            git("worktree", "add", "-b", "fixture-branch", str(project))
            target = project / "target"
            (target / "debug").mkdir(parents=True)
            (target / "CACHEDIR.TAG").write_text("generated cache marker")
            (target / "debug" / "build").write_text("generated build output")
        cache = project / "node_modules"
        cache.mkdir(parents=True)
        (cache / "generated.js").write_text("generated test data")
        old = time.time() - 90 * 86400
        for parent, directories, files in os.walk(root, topdown=False):
            for name in files:
                os.utime(Path(parent) / name, (old, old))
            os.utime(parent, (old, old))
        master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 36, 120, 0, 0))
        original = termios.tcgetattr(slave)
        environment = os.environ.copy()
        for key in ["OPENROUTER_API_KEY", "CLOUDFLARE_API_TOKEN", "CLOUDFLARE_AUTH_TOKEN", "TYPESAFE_API_KEY"]:
            environment.pop(key, None)
        environment["TERM"] = "xterm-256color"
        environment.pop("NO_COLOR", None)
        environment["COLORTERM"] = "truecolor"
        process = subprocess.Popen(
            [str(binary), "tui", "--config", str(root / "empty-config"), "--root", str(root), "--no-docker", "--no-global-caches"],
            stdin=slave, stdout=slave, stderr=slave, cwd="/tmp", env=environment,
        )
        output = bytearray()
        screen = Screen(120, 36)

        def pump(seconds=0.5):
            end = time.monotonic() + seconds
            while time.monotonic() < end:
                if select.select([master], [], [], min(0.05, end-time.monotonic()))[0]:
                    data = os.read(master, 65536)
                    output.extend(data)
                    screen.feed(data)

        def keys(value):
            os.write(master, value)
            pump()

        try:
            for _ in range(30):
                pump(0.1)
                if "READY" in screen.text:
                    break
            assert "READY" in screen.text, f"inventory did not finish; exit={process.poll()} screen={screen.text}"
            if scenario == "worktree":
                assert "2 caches included" in screen.text, screen.text
                assert "INCLUDED WITH WORKTREE REMOVAL" in screen.text, screen.text
                keys(b"\t")
                assert "PART OF WORKTREE" in screen.text, screen.text
                keys(b"\x1b[Z")
            keys(b" a")
            assert "Help / setup" in screen.text, "missing credentials should show setup"
            keys(b"\r")
            keys(b"r")
            assert "Review deletion" in screen.text
            assert "UNREVIEWED" in screen.text
            if scenario == "worktree":
                assert "Includes:" in screen.text, screen.text
            keys(b"yes\r")
            assert cache.exists(), "incorrect approval deleted data"
            keys(b"\x1b")
            keys(b"r")
            keys(b"\x1b")
            assert cache.exists(), "cancel deleted data"
            keys(b"r")
            keys(b"DELETE 1\r")
            for _ in range(30):
                pump(0.1)
                if not cache.exists():
                    break
            assert not cache.exists(), "explicit confirmation did not remove fixture cache"
            assert (source_project / "package.json").exists(), "main source manifest was removed"
            if scenario == "worktree":
                assert not project.exists(), "worktree removal left its checkout behind"
                assert not target.exists(), "worktree removal left build output behind"
                git("show-ref", "--verify", "refs/heads/fixture-branch")
            keys(b"q")
            process.wait(timeout=5)
            assert process.returncode == 0
            assert termios.tcgetattr(slave) == original, "terminal modes were not restored"
            assert b"\x1b[?1049l" in output, "alternate screen was not restored"
            print(f"PTY {scenario} passed: grouping, setup, invalid approval, cancellation, exact approval, source and branch preservation, terminal restoration.")
        finally:
            if process.poll() is None:
                process.kill()
                process.wait()
            os.close(master)
            os.close(slave)
