#!/usr/bin/env python3
"""Check real TUI measurement concurrency with controlled local du latency."""
import fcntl
import json
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


def run(binary, stores, check_overlap=True):
    with tempfile.TemporaryDirectory(prefix="dev-sweep-parallel-test-") as directory:
        root = Path(directory)
        home = root / "home"
        projects = home / ".t3/projects"
        caches = []
        for index in range(32):
            project = projects / f"{index:02}"
            cache = project / "node_modules"
            cache.mkdir(parents=True)
            (project / "package.json").write_text("{}")
            (cache / "generated.js").write_text("disposable generated fixture")
            caches.append(cache)
        for folder in [".android", "Android", ".rustup", ".local/share", ".t3/userdata", ".cache/uv", ".t3/worktrees", ".codex/worktrees"]:
            (home / folder).mkdir(parents=True, exist_ok=True)
        commands = root / "bin"
        commands.mkdir()
        log = root / "measurements.jsonl"
        native_temp = root / "native-temp"
        native_temp.mkdir()
        du = commands / "du"
        du.write_text(f"#!{sys.executable}\n" + '''import json, os, sys, time
from pathlib import Path
path = Path(sys.argv[-1])
def record(kind):
    event = json.dumps({"kind":kind,"time":time.monotonic(),"path":str(path),"pid":os.getpid()}) + "\\n"
    descriptor = os.open(os.environ["DEV_SWEEP_MEASUREMENT_LOG"], os.O_WRONLY|os.O_APPEND|os.O_CREAT, 0o600)
    try: os.write(descriptor,event.encode())
    finally: os.close(descriptor)
record("start")
time.sleep(.35 if path.name == "node_modules" and path.parent.name in {"00","08","16","24"} else .06)
record("end")
print("16\\t" + str(path))
''')
        du.chmod(0o755)
        uv = commands / "uv"
        uv.write_text('#!/bin/sh\nprintf "%s\\n" "$HOME/.cache/uv"\n')
        uv.chmod(0o755)
        environment = dict(os.environ, HOME=str(home), PATH=str(commands), TERM="xterm-256color", DEV_SWEEP_MEASUREMENT_LOG=str(log), TMPDIR=str(native_temp))
        for key in ["OPENROUTER_API_KEY", "CLOUDFLARE_API_TOKEN", "CLOUDFLARE_AUTH_TOKEN", "TYPESAFE_API_KEY", "XDG_CACHE_HOME", "XDG_DATA_HOME", "XDG_CONFIG_HOME", "UV_CACHE_DIR"]:
            environment.pop(key, None)
        master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 36, 120, 0, 0))
        screen = Screen(120, 36)
        (root / "config").write_text(f"root={projects}\nglobal_caches={'true' if stores else 'false'}\ndocker=false\nexclude=/var\n")
        arguments = [str(binary), "tui", "--config", str(root / "config")]
        started = time.monotonic()
        process = subprocess.Popen(arguments, stdin=slave, stdout=slave, stderr=slave, cwd="/tmp", env=environment)
        try:
            deadline = started + 30
            while "READY" not in screen.text and time.monotonic() < deadline:
                if select.select([master], [], [], .05)[0]:
                    screen.feed(os.read(master, 65536))
            assert "READY" in screen.text, screen.text
            elapsed = time.monotonic() - started
            os.write(master, b"q")
            process.wait(timeout=5)
            assert process.returncode == 0, screen.text
            assert all(cache.exists() and (cache.parent / "package.json").exists() for cache in caches)
            events = sorted([json.loads(line) for line in log.read_text().splitlines()], key=lambda event:event["time"])
            active = {}
            peak = store_peak = 0
            measured = set()
            for event in events:
                path = Path(event["path"])
                if event["kind"] == "start":
                    if check_overlap:
                        for other in active.values():
                            assert path != other and path not in other.parents and other not in path.parents, f"overlapping measurements: {path} and {other}"
                    active[event["pid"]] = path
                    measured.add(path)
                    peak = max(peak, len(active))
                    store_peak = max(store_peak, sum(value.name != "node_modules" for value in active.values()))
                else:
                    assert active.pop(event["pid"]) == path
            assert not active, "measurement left running after READY"
            assert set(caches) <= measured, "a directory was skipped"
            return elapsed, peak, store_peak
        finally:
            if process.poll() is None:
                process.kill()
                process.wait()
            os.close(master)
            os.close(slave)


if __name__ == "__main__":
    binary = Path(sys.argv[1] if len(sys.argv) > 1 else "target/debug/dev-sweep").resolve()
    baseline = Path(sys.argv[2]).resolve() if len(sys.argv) > 2 else None
    for stores in [False, True]:
        elapsed, peak, store_peak = run(binary, stores)
        assert peak == 8, f"expected eight simultaneous measurements, got {peak}"
        if stores:
            assert store_peak >= 2, f"large storage areas are still serial: {store_peak}"
        print(f"Parallel measurement {'with stores' if stores else 'projects'}: {elapsed:.3f}s, peak {peak}, store peak {store_peak}; no overlap or deletion.")
        if baseline:
            old_elapsed, old_peak, old_stores = run(baseline, stores, check_overlap=False)
            print(f"Controlled latency baseline: {old_elapsed:.3f}s, peak {old_peak}, store peak {old_stores}; speedup {old_elapsed/elapsed:.2f}x.")
