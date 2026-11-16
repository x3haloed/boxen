#!/usr/bin/env python3
"""Forward archive-free boxen.visual.v1 JSON Lines to existing Boxen sources."""
import argparse
from collections import deque
import json
import os
import subprocess
import sys
import threading
import time

MAX_PENDING_CHANGES = 32
MAX_LINE = 128 * 1024
MAX_PAYLOAD = 8 * 1024
MAX_SNAPSHOT_AGE_MS = 3000


def unix_ms():
    return time.time_ns() // 1_000_000


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("address")
    parser.add_argument("peer")
    parser.add_argument("latest_source")
    parser.add_argument("--ordered-source")
    parser.add_argument("--ordered-field", action="append", default=[],
                        help="Send confirmed changes for this field to the ordered source (repeatable)")
    parser.add_argument("--ordered-sustain-ms", type=int, default=0,
                        help="Delay ordered publication until the field is still observed after this interval")
    parser.add_argument("--boxen", default="boxen")
    parser.add_argument("--report-age", action="store_true")
    parser.add_argument("--evidence-origin", help="HTTPS watcher origin visible from the consumer")
    parser.add_argument("--evidence-curl-config", help="Private curl config path on the consumer host")
    args = parser.parse_args()
    if args.ordered_source == args.latest_source:
        parser.error("latest and ordered sources must be distinct")
    if (args.ordered_field or args.ordered_sustain_ms) and not args.ordered_source:
        parser.error("ordered change selection requires --ordered-source")
    if not 0 <= args.ordered_sustain_ms <= 30_000:
        parser.error("--ordered-sustain-ms ranges from 0 to 30000")
    if args.evidence_origin and not args.evidence_origin.startswith("https://"):
        parser.error("The evidence origin must use HTTPS")
    ordered_fields = set(args.ordered_field)

    def events(action, **fields):
        command = [args.boxen, "events", args.address, args.peer,
                   json.dumps(dict(action=action, **fields), separators=(",", ":"), ensure_ascii=False)]
        result = subprocess.run(command, capture_output=True, timeout=15, check=True)
        return json.loads(result.stdout)

    ready = threading.Condition()
    latest = None
    changes = deque()
    lost_changes = 0
    last_frame = None
    finished = False
    last_input = time.monotonic()
    stop = threading.Event()
    failures = []

    def read_samples():
        nonlocal latest, lost_changes, last_frame, finished, last_input
        candidates = {}
        try:
            while line := sys.stdin.buffer.readline(MAX_LINE + 1):
                if len(line) > MAX_LINE:
                    raise ValueError("sensor record exceeds 128 KiB")
                sample = json.loads(line)
                if sample["schema"] != "boxen.visual.v1" or sample["evidenceRetained"]:
                    raise ValueError("publisher expects archive-free boxen.visual.v1 records")
                session, frame = sample["session"], sample["frame"]
                evidence = sample.get("remoteEvidence")
                if evidence is not None and args.evidence_origin:
                    evidence["origin"] = args.evidence_origin.rstrip("/")
                    if args.evidence_curl_config:
                        evidence["curlConfig"] = args.evidence_curl_config
                if len(json.dumps(sample, ensure_ascii=False).encode()) > MAX_PAYLOAD:
                    sample["fields"] = [
                        {key: value for key, value in field.items() if key != "rows"}
                        for field in sample["fields"]
                    ]
                    sample["ocrRowsOmitted"] = True
                packet = dict(id=f"{session}:{frame}", observed_at_ms=sample["capturedAtUnixMs"],
                              payload=sample)
                with ready:
                    last_input = time.monotonic()
                    latest = packet
                    if args.ordered_source:
                        if last_frame and last_frame[0] == session:
                            gap = max(0, frame - last_frame[1] - 1)
                            lost_changes += gap
                            if gap:
                                candidates.clear()
                        elif last_frame:
                            # A new capture session cannot establish what happened in between.
                            lost_changes += 1
                            candidates.clear()
                        last_frame = (session, frame)
                        transitions = [
                            change for change in sample["changes"]
                            if "previous" in change
                            and (not ordered_fields or change["field"] in ordered_fields)
                        ]
                        payload = {key: sample[key] for key in (
                            "schema", "profile", "session", "frame", "capturedAtUnixMs",
                            "remoteEvidence", "serviceReceivedAtUnixMs"
                        ) if key in sample}
                        selected = {}
                        if args.ordered_sustain_ms:
                            for change in transitions:
                                candidates[change["field"]] = (packet["id"], payload, change)
                            for field in sample["fields"]:
                                candidate = candidates.get(field["id"])
                                if candidate is None:
                                    continue
                                ident, original, change = candidate
                                age = packet["observed_at_ms"] - original["capturedAtUnixMs"]
                                if age > 60_000 or (field["status"] == "observed" and field["value"] != change["value"]):
                                    del candidates[field["id"]]
                                elif age >= args.ordered_sustain_ms and field["status"] == "observed":
                                    selected[f"{ident}:{field['id']}"] = dict(original, changes=[change])
                                    del candidates[field["id"]]
                        elif transitions:
                            selected[packet["id"]] = dict(payload, changes=transitions)
                        for ident, item in selected.items():
                            if len(changes) == MAX_PENDING_CHANGES:
                                changes.popleft()
                                lost_changes += 1
                            changes.append(dict(id=ident, observed_at_ms=item["capturedAtUnixMs"], payload=item))
                    ready.notify_all()
        except (KeyError, ValueError, TypeError) as error:
            failures.append(error)
            stop.set()
        finally:
            with ready:
                finished = True
                ready.notify_all()

    def publish(source_id, mode):
        nonlocal latest, lost_changes
        try:
            source = events("source", id=source_id)
            if source["spec"]["mode"] != mode or not source["enabled"]:
                raise ValueError(f"{mode} source is disabled or has the wrong mode")
            generation = source["generation"]
            sequence = source["producer_seq"]
            while not stop.is_set():
                with ready:
                    ready.wait_for(lambda: stop.is_set() or finished or
                                   (latest is not None if mode == "latest" else changes or lost_changes))
                    if stop.is_set():
                        return
                    if mode == "latest":
                        packet, latest = latest, None
                    elif lost_changes:
                        packet = dict(id=f"loss:{generation}:{sequence + 1}", observed_at_ms=unix_ms(),
                                      payload={"condition": "publisher_gap", "gap_count": lost_changes})
                        lost_changes = 0
                    else:
                        packet = changes.popleft() if changes else None
                    if packet is None:
                        return
                if mode == "latest" and unix_ms() - packet["observed_at_ms"] > MAX_SNAPSHOT_AGE_MS:
                    continue
                if len(json.dumps(packet["payload"], ensure_ascii=False).encode()) > MAX_PAYLOAD:
                    if mode == "ordered":
                        # This keeps a loss notice in the same stream as the later transitions.
                        with ready:
                            lost_changes += 1
                    continue
                request = dict(id=source_id, generation=generation, sequence=sequence + 1,
                               response=dict(protocol="boxen-adapter-v1", observations=[packet]))
                while not stop.is_set():
                    try:
                        receipt = events("publish", **request)
                    except (subprocess.SubprocessError, OSError):
                        # A lost reply can follow acceptance. Reconcile before using the sequence
                        # for any other payload. Current state can be superseded during recovery.
                        if stop.wait(2):
                            return
                        try:
                            current = events("source", id=source_id)
                        except (subprocess.SubprocessError, OSError):
                            continue
                        if not current["enabled"] or current["generation"] != generation:
                            raise ValueError(f"{mode} source generation changed")
                        if current["producer_seq"] not in (sequence, sequence + 1):
                            raise ValueError(f"{mode} source has a different publisher")
                        if current["producer_seq"] == sequence + 1:
                            sequence += 1
                            break
                        if mode == "latest":
                            break
                        continue
                    if receipt.get("producer_seq") != sequence + 1:
                        raise ValueError(f"{mode} publication returned an unexpected receipt")
                    sequence += 1
                    if mode == "latest":
                        age = unix_ms() - packet["observed_at_ms"]
                        if args.report_age:
                            print(f"latest frame {packet['payload']['frame']} acknowledged at age {age} ms",
                                  file=sys.stderr, flush=True)
                        stop.wait(1)
                    break
        except (OSError, subprocess.SubprocessError, ValueError, KeyError) as error:
            failures.append(error)
            stop.set()
            with ready:
                ready.notify_all()

    reader = threading.Thread(target=read_samples, daemon=True)
    workers = [threading.Thread(target=publish, args=(args.latest_source, "latest"))]
    if args.ordered_source:
        workers.append(threading.Thread(target=publish, args=(args.ordered_source, "ordered")))
    reader.start()
    for worker in workers:
        worker.start()
    try:
        while any(worker.is_alive() for worker in workers):
            with ready:
                if not finished and time.monotonic() - last_input > 30:
                    failures.append(TimeoutError("sensor produced no samples for 30 seconds"))
                    stop.set()
                    ready.notify_all()
                    break
            for worker in workers:
                worker.join(0.25)
    except KeyboardInterrupt:
        stop.set()
        with ready:
            ready.notify_all()
        os._exit(130)
    if failures:
        for error in failures:
            reason = str(error) if isinstance(error, (ValueError, TimeoutError)) else type(error).__name__
            print(f"visual publisher stopped: {reason}",
                  file=sys.stderr, flush=True)
        # A daemon reader can still have stdin locked while the sensor is running.
        os._exit(1)


if __name__ == "__main__":
    main()
