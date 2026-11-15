#!/usr/bin/env python3
"""Measure cross-machine mail on three isolated daemons on one host.

A fixed send schedule spans a destination outage and recovery while a second
destination remains healthy. Emit one JSON result for an incumbent or candidate.
No model runtime, credentials, or installed daemon is needed.
"""

import argparse
from collections import Counter
import hashlib
import json
import math
import os
from pathlib import Path
import secrets
import select
import socket
import statistics
import subprocess
import tempfile
import threading
import time
import urllib.request

from smoke_helpers import PipeLines


EVENTS = "mailbox.message_accepted,network.mail_received,network.mail_delivery"


def reserve_ports(count):
    sockets = [socket.socket() for _ in range(count)]
    try:
        for sock in sockets:
            sock.bind(("127.0.0.1", 0))
        return [sock.getsockname()[1] for sock in sockets]
    finally:
        for sock in sockets:
            sock.close()


def poll(check, timeout=45, interval=0.15):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if check():
            return
        time.sleep(interval)
    raise AssertionError(f"Deadline expired: {check.__name__}")


def utc_ms():
    return time.time_ns() // 1_000_000


def p95(samples):
    return sorted(samples)[math.ceil(len(samples) * 0.95) - 1]


class Link:
    """A TCP fault gate that leaves Noise terminated by the actual daemons."""

    def __init__(self, destination):
        self.destination = destination
        self.listener = socket.socket()
        self.listener.bind(("127.0.0.1", 0))
        self.listener.listen()
        self.listener.settimeout(0.1)
        self.address = f"127.0.0.1:{self.listener.getsockname()[1]}"
        self.fault = None
        self.done = threading.Event()
        self.thread = threading.Thread(target=self.accept, daemon=True)
        self.thread.start()

    def accept(self):
        while not self.done.is_set():
            try:
                connection, _ = self.listener.accept()
            except TimeoutError:
                continue
            except OSError:
                return
            threading.Thread(target=self.forward, args=(connection,), daemon=True).start()

    def forward(self, connection):
        with connection:
            try:
                if self.fault == "offline":
                    return
                while self.fault == "stalled":
                    if self.done.wait(0.02):
                        return
                with socket.create_connection(("127.0.0.1", self.destination), timeout=2) as peer:
                    connection.settimeout(2)
                    while not self.done.is_set() and self.fault is None:
                        ready, _, _ = select.select([connection, peer], [], [], 0.1)
                        for origin in ready:
                            data = origin.recv(65536)
                            if not data:
                                return
                            (connection if origin is peer else peer).sendall(data)
            except OSError:
                return

    def close(self):
        self.done.set()
        self.listener.close()
        self.thread.join(timeout=1)


class Machine:
    def __init__(self, command, directory, peer, ui):
        self.command = [*command, "--data-dir", str(directory)]
        self.directory = directory
        self.peer = f"127.0.0.1:{peer}"
        self.ui = f"http://127.0.0.1:{ui}/api/operation"
        self.key = self.cli("identity")
        self.log = directory / "timing.jsonl"
        self.process = None

    def cli(self, *args):
        return subprocess.check_output([*self.command, *args], text=True, timeout=15).strip()

    def start(self, advertised):
        self.process = subprocess.Popen(
            [*self.command, "serve", "--listen", self.peer, "--advertise", advertised,
             "--ui", self.ui.removeprefix("http://").removesuffix("/api/operation")],
            stdout=subprocess.PIPE, text=True,
        )
        lines = PipeLines(self.process.stdout)
        for _ in range(3):
            line = lines.get()
        assert "Local controls:" in line
        self.token = (self.directory / "control-token").read_text().strip()
        self.api({"op": "log_file", "path": str(self.log),
                  "filter": {"events": EVENTS.split(",")}})

    def stop(self):
        if self.process is None:
            return
        self.process.terminate()
        try:
            self.process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            self.process.kill()
            self.process.wait(timeout=5)
        self.process.stdout.close()
        self.process = None

    def api(self, operation):
        request = urllib.request.Request(
            self.ui, headers={"Authorization": f"Bearer {self.token}",
                              "Content-Type": "application/json"},
            data=json.dumps({"peer": self.key, "address": self.peer, "request": operation}).encode(),
        )
        with urllib.request.urlopen(request, timeout=5) as response:
            return json.load(response)

    def records(self):
        self.api({"op": "log_off"})
        return [json.loads(line) for line in self.log.read_text().splitlines()]

    def usage(self):
        cpu, rss = subprocess.check_output(
            ["ps", "-o", "time=", "-o", "rss=", "-p", str(self.process.pid)], text=True,
        ).split()
        parts = cpu.replace("-", ":").split(":")
        units = [1, 60, 3600, 86400]
        return sum(float(part) * unit for part, unit in zip(reversed(parts), units)), int(rss)


def benchmark(args, directory):
    artifact = args.artifact.resolve()
    command = ["/bin/bash", str(artifact)] if artifact.suffix == ".com" else [str(artifact)]
    ports = reserve_ports(6)
    machines = [Machine(command, directory / str(i), ports[i], ports[i + 3]) for i in range(3)]
    source, healthy, impaired = machines
    residents = {name: secrets.token_hex(32) for name in ("healthy", "impaired")}
    client = secrets.token_hex(32)
    sent = {}
    samples = []
    link = Link(ports[2])

    def send(target):
        ident = secrets.token_hex(32)
        message = {"id": ident, "to": {"type": "resident", "id": residents[target]},
                   "subject": "Delivery benchmark", "body": "Synthetic fixture"}
        reply = source.api({"op": "mailbox_send", "client_id": client, "message": message})
        assert reply["id"] == ident and reply["delivery"]["status"] in {"queued", "accepted"}
        sent[ident] = target

    def pending():
        return source.api({"op": "mailbox_status"})["network_pending"]

    def inbox(machine, target):
        return machine.api({"op": "resident_inbox", "id": residents[target]})["messages"]

    try:
        for first, second in [(source, healthy), (healthy, source),
                              (source, impaired), (impaired, source)]:
            first.cli("trust", second.key, "Benchmark peer")
        for machine in machines:
            machine.start(link.address if machine is impaired else machine.peer)
        for name, machine in [("healthy", healthy), ("impaired", impaired)]:
            machine.api({"op": "resident_create", "spec": {
                "id": residents[name], "name": "Benchmark resident", "cwd": str(directory),
                "model": "fixture", "auto_activate": False,
            }})
            source.cli("ping", link.address if machine is impaired else machine.peer,
                       machine.key, "benchmark")
        def catalogs_ready():
            catalog = source.directory / "network-directory"
            return all(
                (path := catalog / f"{machine.key}.json").exists()
                and residents[name] in path.read_text()
                for name, machine in [("healthy", healthy), ("impaired", impaired)]
            )
        poll(catalogs_ready)
        cpu_before, _ = source.usage()
        link.fault = args.fault
        started = time.monotonic()
        for _ in range(args.backlog):
            send("impaired")
        next_send = started
        recover_at = started + args.outage
        until = recover_at + args.after
        recovered = None
        while time.monotonic() < until:
            now = time.monotonic()
            if recovered is None and now >= recover_at:
                recovered = utc_ms()
                link.fault = None
            if now >= next_send:
                send("healthy")
                next_send += args.interval
                samples.append(pending())
            time.sleep(0.02)
        assert recovered is not None
        last_send_ms = utc_ms()

        def all_receipts_retained():
            outstanding = pending()
            samples.append(outstanding)
            return outstanding == 0

        poll(all_receipts_retained, timeout=args.deadline)
        cpu_after, rss = source.usage()
        for target, machine in [("healthy", healthy), ("impaired", impaired)]:
            received = inbox(machine, target)
            assert Counter(row["id"] for row in received) == Counter(
                ident for ident, recipient in sent.items() if recipient == target
            ), f"Lost or duplicate inbox message for {target}"
        accepted, receipts, attempts = {}, {}, Counter()
        for event in source.records():
            ident = event.get("fields", {}).get("message_id")
            assert event["event"] != "logging.dropped", event
            if ident not in sent:
                continue
            if event["event"] == "mailbox.message_accepted":
                assert ident not in accepted
                accepted[ident] = event["timestamp_ms"]
            if event["event"] == "network.mail_delivery":
                attempts[ident] += 1
                if event["fields"]["status"] == "accepted":
                    assert ident not in receipts
                    receipts[ident] = event["timestamp_ms"]

        deliveries = {}
        for machine in (healthy, impaired):
            for event in machine.records():
                assert event["event"] != "logging.dropped", event
                if event["event"] == "network.mail_received":
                    ident = event["fields"]["message_id"]
                    assert ident not in deliveries
                    deliveries[ident] = event["timestamp_ms"]

        assert accepted.keys() == deliveries.keys() == receipts.keys() == sent.keys()
        assert all(accepted[i] <= deliveries[i] <= receipts[i] for i in sent)
        healthy_latency = [deliveries[i] - accepted[i] for i in sent if sent[i] == "healthy"]
        impaired_deliveries = [deliveries[i] for i in sent if sent[i] == "impaired"]
        assert all(at >= recovered - 100 for at in impaired_deliveries)
        return {
            "binary_sha256": hashlib.sha256(artifact.read_bytes()).hexdigest(),
            "fault": args.fault, "outage_seconds": args.outage, "after_seconds": args.after,
            "healthy_interval_seconds": args.interval, "backlog": args.backlog,
            "healthy_count": len(healthy_latency),
            "healthy_p95_ms": p95(healthy_latency),
            "healthy_median_ms": statistics.median(healthy_latency),
            "healthy_max_ms": max(healthy_latency),
            "impaired_first_ms": min(impaired_deliveries) - recovered,
            "impaired_drain_ms": max(impaired_deliveries) - recovered,
            "sender_receipt_p95_ms": p95([receipts[i] - deliveries[i] for i in sent]),
            "attempts": sum(attempts.values()),
            "failed_attempts": sum(attempts.values()) - len(sent),
            "sampled_peak_pending": max(samples),
            "source_cpu_seconds": round(cpu_after - cpu_before, 3),
            "source_final_rss_kib": rss,
            "samples": [{"target": sent[i], "accepted_relative_ms": accepted[i] - recovered,
                         "destination_relative_ms": deliveries[i] - recovered,
                         "receipt_relative_ms": receipts[i] - recovered, "attempts": attempts[i]}
                        for i in sent],
            "send_window_end_relative_ms": last_send_ms - recovered,
        }
    finally:
        link.close()
        for machine in machines:
            machine.stop()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("artifact", type=Path)
    parser.add_argument("--fault", choices=["offline", "stalled"], default="offline")
    parser.add_argument("--outage", type=float, default=9)
    parser.add_argument("--after", type=float, default=16)
    parser.add_argument("--interval", type=float, default=0.4)
    parser.add_argument("--backlog", type=int, default=16)
    parser.add_argument("--deadline", type=float, default=90)
    args = parser.parse_args()
    if os.name != "posix":
        parser.error("the resource probe requires a POSIX host")
    if min(args.outage, args.after, args.interval, args.deadline) <= 0:
        parser.error("all timing values must be positive")
    if not 1 <= args.backlog <= 40 or math.ceil((args.outage + args.after) / args.interval) > 80:
        parser.error("use 1–40 backlog messages and at most 80 healthy sends")
    with tempfile.TemporaryDirectory(prefix="boxen-mail-bench-") as temporary:
        print(json.dumps(benchmark(args, Path(temporary))), flush=True)


if __name__ == "__main__":
    main()
