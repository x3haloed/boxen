#!/usr/bin/env python3
"""Measure the desktop's CPU-side frames against an isolated daemon and a retained fixture."""

import argparse
import json
import os
from pathlib import Path
import socket
import subprocess
import tempfile

from smoke_helpers import PipeLines

ROOT = Path(__file__).resolve().parent.parent
THREAD = "a" * 64


def ports():
    with socket.socket() as peer, socket.socket() as control:
        for listener in (peer, control):
            listener.bind(("127.0.0.1", 0))
        return peer.getsockname()[1], control.getsockname()[1]


def seed(home, owner, runs, replies):
    for index in range(1, runs + 1):
        ident = f"{index:064x}"
        path = home / "runs" / ident
        path.mkdir(parents=True)
        prompt = f"Review the synthetic record for turn {index}."
        (path / "record.json").write_text(json.dumps({
            "owner": owner, "created_at": index,
            "spec": {"id": ident, "thread_id": THREAD, "backend": "codex",
                     "cwd": str(home), "model": "fixture", "prompt": prompt},
        }))
        events = [("user_message", {"text": prompt})]
        for reply in range(replies):
            item = f"reply-{reply}"
            text = (
                f"Turn {index}, update {reply}. **Checked retained input.** "
                "This synthetic paragraph wraps at the desktop's normal conversation width. "
                "The output has stable authorship and appears once in the transcript."
            )
            events += [
                ("reasoning_delta", {"id": f"thought-{reply}", "text": "Inspect the fixture."}),
                ("assistant_delta", {"id": item, "text": text[:45]}),
                ("assistant_message", {"id": item, "text": text}),
            ]
        if index == runs:
            events.append(("assistant_message", {"id": "newest", "text": "Newest fixture message"}))
        events.append(("status", {"status": "completed"}))
        (path / "events.jsonl").write_text("".join(
            json.dumps({"seq": seq, "at": index, "kind": kind, "data": data}) + "\n"
            for seq, (kind, data) in enumerate(events, 1)
        ))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=ROOT / "target/release/boxen")
    parser.add_argument("--runs", type=int, default=240)
    parser.add_argument("--replies", type=int, default=12)
    args = parser.parse_args()
    if not 1 <= args.runs <= 2000 or not 1 <= args.replies <= 100:
        parser.error("use 1–2000 runs and 1–100 replies per run")
    binary = args.binary.resolve()
    command = (["bash"] if binary.suffix == ".com" else []) + [str(binary)]
    with tempfile.TemporaryDirectory(prefix="boxen-desktop-history-") as temporary:
        home = Path(temporary)
        command += ["--data-dir", str(home)]
        owner = subprocess.check_output([*command, "identity"], text=True, timeout=10).strip()
        seed(home, owner, args.runs, args.replies)
        peer, control = ports()
        origin = f"http://127.0.0.1:{control}"
        daemon = subprocess.Popen(
            [*command, "serve", "--listen", f"127.0.0.1:{peer}",
             "--ui", f"127.0.0.1:{control}"],
            stdout=subprocess.PIPE, text=True,
        )
        try:
            lines = PipeLines(daemon.stdout)
            assert "Local controls:" in [lines.get() for _ in range(3)][-1]
            subprocess.run(
                ["cargo", "test", "--release", "--locked", "--manifest-path", "desktop/Cargo.toml",
                 "retained_history_frames", "--", "--ignored", "--nocapture"],
                cwd=ROOT, check=True,
                env=os.environ | {"BOXEN_HISTORY_HOME": str(home), "BOXEN_HISTORY_ORIGIN": origin,
                                  "BOXEN_HISTORY_THREAD": THREAD,
                                  "BOXEN_HISTORY_RUNS": str(args.runs),
                                  "BOXEN_HISTORY_REPLIES": str(args.replies)},
            )
        finally:
            daemon.terminate()
            try:
                daemon.wait(timeout=5)
            except subprocess.TimeoutExpired:
                daemon.kill()
                daemon.wait()
            daemon.stdout.close()


if __name__ == "__main__":
    main()
