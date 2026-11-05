#!/usr/bin/env python3
"""Bootstrap two LAN peers over SSH, then verify direct Boxen traffic both ways.

Development helper for macOS/Linux hosts with SSH key authentication, not an
end-user installer. Leaves both foreground-capable daemons running via nohup.
Use on hosts without an existing Boxen daemon on the chosen ports.
"""
import argparse
import json
import os
import pathlib
import shlex
import socket
import subprocess
import time

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("host", help="SSH destination, e.g. user@example-host")
parser.add_argument("--binary", default="target/cosmo/release/boxen.com")
parser.add_argument("--local-address", help="LAN address the remote machine can reach")
parser.add_argument("--port", type=int, default=42424)
parser.add_argument("--ui-port", type=int, default=42425)
args = parser.parse_args()
binary = pathlib.Path(args.binary).resolve()
if not binary.is_file():
    parser.error("build the binary first")
if not 1 <= args.port <= 65535 or not 1 <= args.ui_port <= 65535:
    parser.error("ports must be in 1..65535")
if args.port == args.ui_port:
    parser.error("peer and UI ports must differ")


def ssh(command):
    return subprocess.check_output(["ssh", "-o", "BatchMode=yes", "-o", "ConnectTimeout=10",
                                    args.host, command], text=True).strip()


remote_home = ssh('printf "%s" "$HOME"')
remote_root = remote_home + "/.boxen"
remote_binary = remote_root + "/bin/" + binary.name
remote_command = (["/bin/bash"] if binary.suffix == ".com" else []) + [remote_binary]
local_root = pathlib.Path(os.environ.get("BOXEN_HOME", pathlib.Path.home() / ".boxen"))
local_root.mkdir(mode=0o700, parents=True, exist_ok=True)
local_command = (["/bin/bash"] if binary.suffix == ".com" else []) + [str(binary), "--data-dir", str(local_root)]

# Refuse to replace a live installation. A successful bind also checks the UI port.
with socket.socket() as peer_probe, socket.socket() as ui_probe:
    peer_probe.bind(("0.0.0.0", args.port))
    ui_probe.bind(("127.0.0.1", args.ui_port))
remote_probe = "if lsof -nP -iTCP:{0} -sTCP:LISTEN >/dev/null 2>&1 || lsof -nP -iTCP:{1} -sTCP:LISTEN >/dev/null 2>&1; then exit 1; fi".format(args.port, args.ui_port)
ssh(remote_probe)
ssh("umask 077; mkdir -p " + shlex.quote(remote_root + "/bin"))
subprocess.run(["scp", "-q", str(binary), args.host + ":" + shlex.quote(remote_binary)], check=True)
ssh("chmod 700 " + shlex.quote(remote_binary))
local_key = subprocess.check_output(local_command + ["identity"], text=True).strip()
remote_key = ssh(shlex.join(remote_command + ["identity"]))
subprocess.run(local_command + ["trust", remote_key, args.host], check=True)
ssh(shlex.join(remote_command + ["trust", local_key, socket.gethostname()]))

remote_log = remote_root + "/daemon.log"
remote_pid = remote_root + "/daemon.pid"
launch = remote_command + ["serve", "--listen", f"0.0.0.0:{args.port}", "--ui", f"127.0.0.1:{args.ui_port}", "--name", args.host.rsplit("@",1)[-1]]
ssh("umask 077; nohup " + shlex.join(launch) + " </dev/null >" + shlex.quote(remote_log)
    + " 2>&1 & echo $! >" + shlex.quote(remote_pid))
log = local_root / "daemon.log"
descriptor = os.open(log, os.O_CREAT | os.O_WRONLY | os.O_TRUNC, 0o600)
with os.fdopen(descriptor, "w") as output:
    process = subprocess.Popen(local_command + ["serve", "--listen", f"0.0.0.0:{args.port}",
                               "--ui", f"127.0.0.1:{args.ui_port}", "--name", socket.gethostname()], stdin=subprocess.DEVNULL,
                               stdout=output, stderr=subprocess.STDOUT, start_new_session=True)
(local_root / "daemon.pid").write_text(str(process.pid) + "\n")

hostname = args.host.rsplit("@", 1)[-1]
local_address = args.local_address
if not local_address:
    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as route:
        route.connect((hostname, args.port))
        local_address = route.getsockname()[0]
for attempt in range(20):
    probe = subprocess.run(local_command + ["ping", f"{hostname}:{args.port}", remote_key,
                                            "Hello from the local box"], capture_output=True, text=True)
    if probe.returncode == 0:
        break
    time.sleep(0.25)
else:
    raise RuntimeError("remote exchange failed; inspect daemon.log on both hosts: " + probe.stderr)
print(probe.stdout.strip())
print(ssh(shlex.join(remote_command + ["ping", f"{local_address}:{args.port}", local_key,
                                      "Hello from the remote box"])))
settings = {"ssh_host": args.host, "remote_address": f"{hostname}:{args.port}",
            "local_address": f"{local_address}:{args.port}", "remote_key": remote_key,
            "local_key": local_key, "remote_binary": remote_binary}
(local_root / "lan-peer.json").write_text(json.dumps(settings, indent=2) + "\n")
print("Paired and verified direct traffic in both directions. Daemons remain running.")
print("Local control URL is in " + str(log) + "; remote control URL is in " + remote_log)
print("No login/startup service was installed. PID files are beside the logs.")
