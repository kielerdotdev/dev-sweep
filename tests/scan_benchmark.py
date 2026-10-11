#!/usr/bin/env python3
"""Time a read-only TUI scan over many generated dependency folders."""
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

binary = Path(sys.argv[1] if len(sys.argv) > 1 else 'target/release/dev-sweep').resolve()
with tempfile.TemporaryDirectory(prefix='dev-sweep-benchmark-') as directory:
    root = Path(directory)
    for index in range(80):
        project = root / str(index)
        cache = project / 'node_modules'
        cache.mkdir(parents=True)
        (project / 'package.json').write_text('{}')
        for file in range(64):
            (cache / str(file)).write_text('generated fixture data')
    master, slave = pty.openpty()
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', 36, 120, 0, 0))
    screen = Screen(120, 36)
    started = time.monotonic()
    process = subprocess.Popen(
        [str(binary), 'tui', '--config', str(root / 'config'), '--root', str(root), '--no-docker', '--no-global-caches'],
        stdin=slave, stdout=slave, stderr=slave, cwd='/tmp', env=dict(os.environ, TERM='xterm-256color'),
    )
    try:
        while 'READY' not in screen.text and time.monotonic() - started < 60:
            if select.select([master], [], [], .05)[0]:
                screen.feed(os.read(master, 65536))
        assert 'READY' in screen.text, screen.text
        assert '80 items' in screen.text, screen.text
        print(f'{binary}: {time.monotonic() - started:.3f}s to complete 80-project inventory')
        os.write(master, b'q')
        process.wait(timeout=5)
        assert process.returncode == 0
        assert all((root / str(i) / 'node_modules').exists() for i in range(80))
    finally:
        if process.poll() is None:
            process.kill()
            process.wait()
        os.close(master)
        os.close(slave)
