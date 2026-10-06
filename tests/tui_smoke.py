#!/usr/bin/env python3
"""Exercise the real TUI in a PTY with an isolated Git config and fake gh."""
import fcntl
import json
import os
import re
from pathlib import Path
import select
import struct
import subprocess
import sys
import tempfile
import termios
import time
import unicodedata

BINARY = Path(sys.argv[1] if len(sys.argv) > 1 else "target/debug/gitsw").resolve()


def rendered_history(data):
    """Track the cursor/line commands used by the inline picker."""
    lines = [[]]
    row = column = 0
    for token in re.findall(r"\x1b\[[0-9;?]*[A-Za-z]|[^\x1b]", data.decode(errors="replace")):
        if token.startswith("\x1b["):
            code = token[-1]
            value = token[2:-1]
            if code == "A":
                row = max(0, row - int(value or "1"))
            elif code == "G":
                column = int(value or "1") - 1
            elif code == "K":
                lines[row] = []
        elif token == "\r":
            column = 0
        elif token == "\n":
            row += 1
            while len(lines) <= row:
                lines.append([])
        elif token.isprintable():
            while len(lines[row]) <= column:
                lines[row].append(" ")
            lines[row][column] = token
            column += 1
    return "".join(lines[0])


def scenario(keys, expected_name, rows=24, cols=90, github_active="Bob", args=(), check_profiles=None, expected_text=(), followup=False):
    with tempfile.TemporaryDirectory() as raw:
        root = Path(raw)
        config = root / "config/gitsw"
        config.mkdir(parents=True)
        profiles = [{"label": "personal", "name": "Alice", "email": "alice@example.com", "github": {"host": "github.com", "user": "Alice"}},
                    {"label": "工作", "name": "Bob", "email": "bob@example.com", "github": {"host": "github.com", "user": "Bob"}}]
        (config / "profiles.json").write_text(json.dumps({"profiles": profiles}))
        git = root / ".gitconfig"
        git.write_text("[user]\n name = Alice\n email = alice@example.com\n")
        bin_dir = root / "bin"
        bin_dir.mkdir()
        gh = bin_dir / "gh"
        status = {"hosts": {"github.com": [{"login": user, "active": user == github_active, "tokenSource": "keyring"} for user in ("Alice", "Bob")]}}
        gh.write_text("#!/bin/sh\nif [ \"$1 $2\" = 'auth status' ]; then\nprintf '%s' '" + json.dumps(status) + "'\nfi\n")
        gh.chmod(0o755)
        env = dict(os.environ, HOME=raw, XDG_CONFIG_HOME=str(root / "config"),
                   GIT_CONFIG_GLOBAL=str(git), GIT_CONFIG_NOSYSTEM="1",
                   PATH=str(bin_dir) + os.pathsep + os.environ["PATH"], TERM="xterm-256color")
        for key in ("GIT_CONFIG_COUNT", "GIT_CONFIG_PARAMETERS", "NO_COLOR"):
            env.pop(key, None)
        master, slave = os.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", rows, cols, 0, 0))

        def terminal_session():
            os.setsid()
            fcntl.ioctl(slave, termios.TIOCSCTTY, 0)

        # Keep the controlling session alive long enough to inspect the TUI's restored modes.
        wrapper = """import subprocess, sys, termios
print('HISTORY_BEFORE_GITSW', flush=True)
before = termios.tcgetattr(0)
result = subprocess.run(sys.argv[1:])
assert termios.tcgetattr(0) == before, 'terminal mode was not restored'
print('TERMINAL_RESTORED', flush=True)
sys.exit(result.returncode)
"""
        process = subprocess.Popen([sys.executable, "-c", wrapper, str(BINARY), *args], stdin=slave, stdout=slave, stderr=slave,
                                   cwd=raw, env=env, preexec_fn=terminal_session)
        screen = bytearray()
        try:
            deadline = time.monotonic() + 8
            while b"\x1b[?25l" not in screen and time.monotonic() < deadline:
                if select.select([master], [], [], 0.1)[0]:
                    screen.extend(os.read(master, 65536))
            assert b"\x1b[?25l" in screen, screen.decode(errors="replace")
            # Wait for initial draw so all keystrokes are processed by the picker.
            time.sleep(0.1)
            if isinstance(keys, bytes):
                os.write(master, keys)
            else:
                for chunk in keys:
                    os.write(master, chunk)
                    until = time.monotonic() + 0.2
                    while time.monotonic() < until:
                        if select.select([master], [], [], 0.02)[0]:
                            chunk_output = os.read(master, 65536)
                            if not chunk_output:
                                break
                            screen.extend(chunk_output)
            while process.poll() is None and time.monotonic() < deadline:
                if select.select([master], [], [], 0.1)[0]:
                    screen.extend(os.read(master, 65536))
            assert process.poll() is not None, screen.decode(errors="replace")
            assert process.wait(timeout=2) == 0, screen.decode(errors="replace")
            while select.select([master], [], [], 0)[0]:
                chunk = os.read(master, 65536)
                if not chunk:
                    break
                screen.extend(chunk)
            assert b"\x1b[?1049" not in screen, "picker used the alternate screen"
            assert b"\x1b[2J" not in screen and b"\x1b[3J" not in screen, "picker cleared terminal history"
            assert not re.search(rb"\x1b\[[0-9;]*[Hf]", screen), "picker used absolute cursor positioning"
            assert rendered_history(screen) == "HISTORY_BEFORE_GITSW", "picker overwrote previous terminal output"
            decoded = screen.decode(errors="replace")
            assert "[Git]" not in decoded and "[gh]" not in decoded, "old badges remain"
            indicator_columns = set()
            for line in decoded.split("\r\n"):
                plain = re.sub(r"\x1b\[[0-9;?]*[A-Za-z]", "", line)
                if "┃" not in plain or not re.match(r"[ >] (personal|工作)", plain):
                    continue
                prefix = plain[:plain.index("┃")]
                column = sum(2 if unicodedata.east_asian_width(c) in "WF" else 1 for c in prefix)
                # The blue bar has its own reserved position after the yellow one.
                if "工作" in plain and github_active == "Bob":
                    column -= 1
                indicator_columns.add(column)
                if "personal" in plain:
                    assert "\x1b[38;5;11m┃" in line, "commit indicator is not yellow"
                    assert plain.count("┃") == (2 if github_active == "Alice" else 1)
                if "工作" in plain and github_active == "Bob":
                    assert "\x1b[38;5;12m┃" in line, "GitHub indicator is not blue"
                    assert plain.count("┃") == 1
            if not args:
                assert len(indicator_columns) == 1, "indicators shifted between accounts or search states"
            for text in expected_text:
                assert text in decoded, (text, decoded)
            assert b"\x1b[?25h" in screen, "cursor was not restored"
            assert b"TERMINAL_RESTORED" in screen, "terminal mode was not restored"
            name = subprocess.check_output(["git", "config", "--file", str(git), "--get", "user.name"], env=env).decode().strip()
            assert name == expected_name, (name, expected_name)
            if followup:
                result = subprocess.run([str(BINARY), "list"], cwd=raw, env=env, capture_output=True)
                assert result.returncode == 0, result.stderr
            if check_profiles:
                check_profiles(json.loads((config / "profiles.json").read_text()))
        finally:
            if process.poll() is None:
                process.kill()
                process.wait()
            os.close(master)
            os.close(slave)


scenario(b"q", "Alice")
scenario(b"\x03", "Alice")
scenario(b"\x1b[B\r", "Bob")
scenario("/bob\r\r".encode(), "Bob")
scenario(b"/nothing\r\x1bq", "Alice")
scenario(b"\rq", "Alice", rows=8, cols=25)
scenario(b"q", "Alice", github_active="Alice")
print("7 inline PTY scenarios passed; colored indicators aligned, terminal restored, previous output preserved.")


def edited(data):
    profile = next(p for p in data["profiles"] if p["label"] == "personal")
    assert profile["name"] == "Alice Updated"
    assert profile["email"] == "updated@example.com"
    assert profile["github"]["user"] == "Bob"


def unchanged(data):
    profile = next(p for p in data["profiles"] if p["label"] == "personal")
    assert profile["name"] == "Alice"
    assert profile["email"] == "alice@example.com"
    assert profile["github"]["user"] == "Alice"


def created(data):
    profile = next(p for p in data["profiles"] if p["label"] == "extra")
    assert profile["name"] == "Extra Person"
    assert profile["email"] == "new@example.com"
    assert profile["github"]["user"] == "Bob"


def deleted(data):
    assert all(p["label"] != "personal" for p in data["profiles"])
    assert any(p["label"] == "personal" for p in data["hidden_profiles"])


def restored(data):
    unchanged(data)
    assert not data.get("hidden_profiles")


DOWN = b"\x1b[B"
UP = b"\x1b[A"
CLEAR = b"\x15"
ENTER = b"\r"
# Edit name/email and select a different gh account, then save.
scenario(ENTER + DOWN + ENTER + CLEAR + b"Alice Updated\r" + DOWN + ENTER + CLEAR + b"updated@example.com\r" + DOWN + ENTER + DOWN + ENTER + DOWN + ENTER + b"q", "Alice", args=("-setting",), check_profiles=edited, expected_text=("Saved personal",))
# Escape discards all unsaved form changes.
scenario([ENTER + DOWN + ENTER + CLEAR + b"Unsaved\r", b"\x1b", b"q"], "Alice", args=("--setting",), check_profiles=unchanged)
# Create a profile; an invalid email keeps the form open until corrected.
scenario(DOWN * 2 + ENTER * 2 + b"extra\r" + DOWN + ENTER + b"Extra Person\r" + DOWN + ENTER + b"bad\r" + DOWN * 2 + ENTER + UP * 2 + ENTER + CLEAR + b"new@example.com\r" + DOWN + ENTER + DOWN * 2 + ENTER + DOWN + ENTER + b"q", "Alice", args=("-setting",), check_profiles=created, expected_text=("email must contain @", "Saved extra"))
DELETE = ENTER + b"\x1b[F" + UP + ENTER + DOWN + ENTER
scenario(DELETE + b"q", "Alice", args=("-setting",), check_profiles=deleted, followup=True)
scenario(DELETE + DOWN * 2 + ENTER * 2 + DOWN * 4 + ENTER + b"q", "Alice", args=("-setting",), check_profiles=restored)
scenario(b"q", "Alice", args=("-s",), check_profiles=unchanged)
print("6 settings PTY scenarios passed: save/link, discard, validation/create, delete, restore, flag aliases.")
