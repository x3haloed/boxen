#!/usr/bin/env python3
"""Admit selected visual SSE changes to a resident-core SQLite event journal."""
import argparse
import json
import os
from pathlib import Path
import sqlite3
import subprocess
import time

MAX_LINE = 256 * 1024


def now_ms():
    return time.time_ns() // 1_000_000


def events(stream):
    event = {}
    while line := stream.readline(MAX_LINE + 1):
        if len(line) > MAX_LINE:
            raise ValueError("visual event exceeds byte limit")
        line = line.rstrip(b"\r\n")
        if not line:
            if "data" in event:
                yield event
            event = {}
            continue
        key, _, value = line.partition(b":")
        if key in (b"id", b"event", b"data"):
            event[key.decode()] = value.lstrip().decode()


def run(args, db):
    db.execute("CREATE TABLE IF NOT EXISTS visual_sse_cursor (source TEXT PRIMARY KEY, cursor TEXT NOT NULL, candidates TEXT NOT NULL)")
    state = dict(state="connecting", lastCollectionMs=None, lastReceiptMs=None, captureHealth=None,
                 lastAdmissionMs=None, lastGap=None, cursor=None, suppressedWakes=0)
    saved = db.execute("SELECT cursor,candidates FROM visual_sse_cursor WHERE source=?", (args.url,)).fetchone()
    candidates = {field: item for field, item in (json.loads(saved[1]).items() if saved else ())
                  if field in args.field}
    fields = set(args.field)

    def report():
        pending = args.status.with_suffix(".tmp")
        pending.write_text(json.dumps(state) + "\n")
        pending.replace(args.status)

    while args.expires_ms > now_ms() and not args.stop.exists():
        state.update(state="connecting", cursor=saved[0] if saved else None)
        report()
        seconds = min(25, max(1, (args.expires_ms - now_ms()) / 1000))
        command = ["curl", "-fsSN", "--config", str(args.curl_config), "--max-time", str(seconds)]
        if saved:
            command += ["-H", "Last-Event-ID: " + saved[0]]
        with subprocess.Popen(command + [args.url.rstrip("/") + "/v1/events"],
                              stdout=subprocess.PIPE, stderr=subprocess.DEVNULL) as process:
            try:
                for message in events(process.stdout):
                    if args.stop.exists() or args.expires_ms <= now_ms():
                        break
                    kind = message.get("event")
                    data = json.loads(message["data"])
                    cursor = message.get("id")
                    prior_candidates = dict(candidates)
                    admissions = []
                    received = now_ms()
                    state.update(state="connected", lastReceiptMs=received)
                    if kind == "gap":
                        candidates.clear()
                        state["lastGap"] = dict(data, receivedAtUnixMs=received)
                    if kind == "health":
                        state["captureHealth"] = data
                    if kind == "change" and cursor:
                        for change in data.get("changes", []):
                            field = change["field"]
                            if field in fields and 0 <= received - data["capturedAtUnixMs"] <= 60_000:
                                candidates[field] = (cursor, data, change, received)
                    if kind == "snapshot":
                        state["captureHealth"] = data["health"]
                        snapshot = data.get("snapshot")
                        if snapshot and data["health"]["state"] == "sampling":
                            state["lastCollectionMs"] = received
                            observed = {f["id"]: f.get("value") for f in snapshot["fields"]
                                        if f["status"] == "observed" and not f.get("carried")}
                            for field, (event_id, event, change, event_receipt) in list(candidates.items()):
                                if received - event_receipt > 60_000 or snapshot["session"] != event["session"]:
                                    del candidates[field]
                                    continue
                                if snapshot["capturedAtUnixMs"] < event["capturedAtUnixMs"] + args.sustain_ms:
                                    continue
                                if field not in observed:
                                    continue
                                del candidates[field]
                                if observed[field] != change["value"]:
                                    continue
                                body = dict(source="visual_sensor", sensorOrigin=args.url, eventId=event_id,
                                            capturedAtUnixMs=event["capturedAtUnixMs"],
                                            serviceReceivedAtUnixMs=event.get("serviceReceivedAtUnixMs"),
                                            receivedAtUnixMs=event_receipt, queuedAtUnixMs=received,
                                            session=event["session"], frame=event["frame"], change=change,
                                            cropPath=next((c["path"] for c in event.get("remoteEvidence", {}).get("crops", [])
                                                           if c["field"] == field), None),
                                            freshPath="/v1/fresh/" + field,
                                            curlConfig=str(args.curl_config),
                                            evidence=event.get("remoteEvidence"))
                                model_input = json.dumps([dict(type="text", text=json.dumps(body), text_elements=[])])
                                admissions.append((f"visual-sse:{args.url}:{event_id}:{field}", model_input))
                    try:
                        with db:
                            db.execute("BEGIN IMMEDIATE")
                            if admissions:
                                busy = db.execute("SELECT 1 FROM events WHERE id LIKE 'visual-sse:%' "
                                                  "AND state IN ('pending','executing','uncertain')").fetchone()
                                if busy or received - (state["lastAdmissionMs"] or 0) < args.wake_interval_ms:
                                    state["suppressedWakes"] += len(admissions)
                                else:
                                    result = db.execute("INSERT OR IGNORE INTO events(id,input) VALUES (?,?)", admissions[0])
                                    if result.rowcount:
                                        state["lastAdmissionMs"] = received
                                    state["suppressedWakes"] += len(admissions) - 1
                            next_cursor = cursor or (saved[0] if saved else "")
                            if next_cursor:
                                db.execute("INSERT INTO visual_sse_cursor VALUES (?,?,?) "
                                           "ON CONFLICT(source) DO UPDATE SET cursor=excluded.cursor,candidates=excluded.candidates",
                                           (args.url, next_cursor, json.dumps(candidates)))
                    except sqlite3.Error:
                        candidates = prior_candidates
                        raise
                    if next_cursor:
                        saved = (next_cursor,)
                        state["cursor"] = next_cursor
                    report()
            except (OSError, ValueError, KeyError, sqlite3.Error):
                state["state"] = "retrying"
            finally:
                if process.poll() is None:
                    process.terminate()
        time.sleep(1)
    state["state"] = "stopped" if args.stop.exists() else "expired"
    report()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--url", required=True)
    parser.add_argument("--curl-config", type=Path, required=True)
    parser.add_argument("--journal", type=Path, required=True)
    parser.add_argument("--status", type=Path, required=True)
    parser.add_argument("--stop", type=Path, required=True)
    parser.add_argument("--expires-ms", type=int, required=True)
    parser.add_argument("--field", action="append", default=[])
    parser.add_argument("--sustain-ms", type=int, default=3000)
    parser.add_argument("--wake-interval-ms", type=int, default=30000)
    args = parser.parse_args()
    if not args.url.startswith("https://") or args.sustain_ms < 0 or args.wake_interval_ms < 0:
        parser.error("An HTTPS URL and nonnegative intervals are required.")
    os.umask(0o077)
    with sqlite3.connect(args.journal.resolve().as_uri() + "?mode=rw", uri=True, timeout=5) as db:
        run(args, db)


if __name__ == "__main__":
    main()
