#!/usr/bin/env python3
"""Verify quitting cancels every parallel native measurement worker."""
import fcntl
import os
from pathlib import Path
import pty
import struct
import subprocess
import sys
import tempfile
import termios
import time

binary = Path(sys.argv[1] if len(sys.argv) > 1 else 'target/debug/dev-sweep').resolve()
with tempfile.TemporaryDirectory(prefix='dev-sweep-cancel-') as directory:
    root = Path(directory)
    projects = root / 'projects'
    for index in range(12):
        project = projects / str(index)
        (project / 'node_modules').mkdir(parents=True)
        (project / 'package.json').write_text('{}')
    commands = root / 'bin'
    commands.mkdir()
    du = commands / 'du'
    du.write_text('#!/bin/sh\necho $$ >> "$DEV_SWEEP_TEST_PIDS"\nsleep 30\n')
    du.chmod(0o755)
    pids = root / 'pids'
    environment = dict(os.environ, TERM='xterm-256color', PATH=str(commands) + ':' + os.environ['PATH'], DEV_SWEEP_TEST_PIDS=str(pids))
    master, slave = pty.openpty()
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', 36, 120, 0, 0))
    process = subprocess.Popen([str(binary), 'tui', '--config', str(root / 'config'), '--root', str(projects), '--no-docker', '--no-global-caches'], stdin=slave, stdout=slave, stderr=slave, cwd='/tmp', env=environment)
    try:
        deadline = time.monotonic() + 10
        workers = []
        while time.monotonic() < deadline:
            workers = pids.read_text().splitlines() if pids.exists() else []
            if len(workers) == 8:
                break
            time.sleep(.02)
        assert len(workers) == 8, f'expected eight active workers, got {workers}'
        started = time.monotonic()
        os.write(master, b'q')
        process.wait(timeout=5)
        assert process.returncode == 0
        for worker in workers:
            try:
                os.kill(int(worker), 0)
            except ProcessLookupError:
                continue
            raise AssertionError(f'left measurement worker {worker} running')
        assert all((projects / str(i) / 'node_modules').exists() for i in range(12))
        print(f'PTY cancellation passed: all eight workers stopped in {time.monotonic() - started:.3f}s; folders preserved.')
    finally:
        if process.poll() is None:
            process.kill()
            process.wait()
        os.close(master)
        os.close(slave)
