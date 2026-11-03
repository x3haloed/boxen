# Platform testing rotation

Runtime and packaging changes are checked with native and portable builds on
authorized development hosts. Hostnames, accounts and machine inventory belong
in private local configuration.

Build the same portable file with the optional transport probe enabled:

```sh
COSMO_HOME="$PWD/work/cosmo" cargo build --locked --release --features ape,inference-probe
python3 scripts/test-linux.py user@example-host
```

The SSH target is supplied as the first argument or through the local
`BOXEN_TEST_LINUX_HOST` environment variable. No deployment host is built in. `--binary PATH` selects another built APE, `--peer-port PORT`
selects the Linux direct-test port (default 42426), and `--output PATH` selects the
local evidence directory. The host needs passwordless SSH,
Python 3, Bash, tar and OpenSSL. Rust, Cargo, Node, an installed Boxen service,
Codex, and model credentials are not prerequisites. The runner checks for the
optional probe commands before uploading rather than silently skipping tests.

An optional `--codex /absolute/linux/path/to/codex --codex-model MODEL` keeps the
eighteen controlled suites and changes the final direct-LAN check to a real Codex
app-server workload and native thread follow-up. This opt-in path uses the Linux
host's existing sign-in and consumes real model usage. The test does not install
a CLI or copy authentication material. Its report distinguishes real Codex from
the controlled fixture and records whether command-output deltas were observed.
See [headless Codex setup and live evidence](workloads.md#headless-codex-on-linux).

Only the binary, selected test scripts, and controlled Codex fixture are copied
to a private `/tmp/boxen-linux-*` directory. The worker records the actual OS,
architecture, kernel, distribution, Python version, binary SHA-256, suite exit
statuses and timings. These eighteen suites run against the uploaded file:

- Blocking, notification and idle CPU checks (`platform`).
- Pairing, authenticated controls and revocation (`smoke`).
- Workload streaming, control, ownership and restart (`workload`).
- Text-history source roles, next-input exposure and effect-free import (`transcript-import`).
- Independent resident definition instances and manual revision application (`resident-class`).
- Native Codex thread import, retained historical replay and same-session continuation (`thread-import`).
- TCP partition/reconnect, retained replay and uncertainty (`recovery`).
- Resident identity, continuity and recovery (`resident`).
- Native-preserving promotion, scoped MCP tools and selective correspondence (`resident-promotion`).
- Selective delivery, no-send completion and batch recovery (`mailbox`).
- Instance-scoped messaging connectors and uncertain external effects (`messaging`).
- Hot logging filters, file/stream sinks and failure behavior (`logging`).
- Resident/thread scheduled wakes, coalescing, crash recovery and idle CPU (`scheduler`).
- External observations/adapters, bounded failures, lifecycle, effects and storage (`events`).
- Built-in inference, actual shell effects and communication (`inference-runtime`).
- Selectable context policies, exact history, review, summary failure and recovery (`compaction`).
- Native media wire bytes, quotas and snapshot recovery (`media`).
- SSE/native-state projection and TLS rejection (`inference-probe`).

A failed suite stops the rotation, retains its local log/report, and returns a
nonzero status. A suite has a 180-second deadline. This is a controlled-provider
rotation: it does not copy OpenRouter keys or make paid model requests.

After the Linux suites pass, temporary peers on the development machine and
Linux exchange authenticated messages in both directions directly over the LAN.
The Linux test peer listens on IPv4 TCP 42426; the Mac peer listens on its routed
LAN IPv4 address and a temporary port. The runner fails if direct connectivity
fails; it does not fall back to SSH forwarding.
The development machine then submits a controlled Codex-fixture workload to
Linux, observes command output before completion, and checks contiguous event
sequences and exact replay. The controlled direct-LAN check also schedules a
follow-up on that Linux thread, verifies its conventional response and native
session continuity, and replays its retained scheduled events. It also transfers a published observation
into that existing Linux thread and verifies its accepted input and consumer position. Only public pairing information crosses the SSH
control stream; temporary GUI control tokens are not printed or retained in
logs. GUI listeners stay on loopback at temporary ports. SSH transfers the bundle
and controls test-process lifetime, but does not carry Boxen peer traffic.
Production daemons, identities and pairing files are not changed. The temporary peers stop and the remote bundle is
removed; cleanup status is included in the result.

For the current trusted LAN, the user configured UFW to allow TCP 42424 (normal
Boxen peers) and TCP 42426 (direct tests) from `192.168.128.0/24`. The runner does
not alter firewall rules or start a persistent daemon on 42424. A different host
or test port needs an appropriate existing rule and an available listener port.
The GUI port does not need an inbound firewall exception.

Evidence is saved locally under `work/linux/UTC_TIMESTAMP/{suites.log,result.json}`.
The JSON includes Linux suite results and the cross-host result, explicitly labeled `direct_tcp` with
`direct_lan_verified: true` only after successful verification. A matching
binary hash establishes that the tested Linux x86-64 executable is the same file
used by the macOS ARM64 tests, rather than a separate Linux build.

## First Linux verification — October 31, 2026

The physical test host runs Debian GNU/Linux 13.7 (trixie), kernel
`6.12.113+deb13-amd64`, on x86-64, with Python 3.13.5. The existing APE initially
passed platform, pairing and workload checks, then failed reconnection.

That failure exposed a client bug: the watcher treated any `BrokenPipe` as a
closed stdout consumer. Linux can also report a broken TCP connection as EPIPE,
so the watcher exited successfully instead of reconnecting. The fix limits clean
consumer shutdown to an error observed while writing stdout; socket errors
continue through the normal reconnect path. The recovery suite now checks both
network reconnection and clean exit after stdout closes. The old binary failed
this test on Linux; the fixed artifact passed on Linux and macOS.

The fixed APE passed all ten controlled Linux suites. Linux's blocking probe
observed 200 ms condition-variable/channel timeouts, one notification wake, and
about 0.007 seconds of CPU during 0.46 seconds of wall time. The native and APE
macOS recovery/blocking checks, Rust unit tests, formatting and Clippy also passed.

Those initial checks added Linux x86-64 APE fixture evidence. They did not verify
real Codex model execution; the subsequent [headless Codex live checks](workloads.md#linux-live-evidence)
now cover that path. Other Linux distributions, Linux ARM64, Windows, macOS
x86-64, native Linux Rust builds, and live OpenRouter execution on Linux remain
unverified. Model perception tests previously run on macOS remain separate from
the exact-wire Linux media fixture checks.

The first direct cross-host probe also found that inbound temporary TCP listeners
on Linux were unreachable from the Mac. A plain Python listener on `0.0.0.0`
timed out too, so this is not established as a Boxen transport defect. The
initial rotation left host network policy alone and used explicit SSH forwards
for its cross-host proof. That first tunneled result did not verify direct
Mac-to-Linux LAN reachability.

The completed rotation used APE SHA-256
`551ee61602e0f7a8f04d443bd6711ccefb644b166ce0937b67b36e13c384df51`.
Both-direction messages and the Linux fixture workload passed over the explicit
SSH forwards. The Mac observed tool output before completion and exactly replayed
all 46 retained events. The report marked remote cleanup successful. The
production Boxen installation and the Linux host's network policy were unchanged.

## Direct LAN verification after the firewall exception

After the user added the trusted-LAN UFW exceptions, the rotation switched to
Linux TCP 42426 and direct peer traffic. The same APE passed all ten suites,
bidirectional authenticated messages between `192.168.128.116` and
`192.168.128.91:42426`, streamed Linux fixture output before completion, and exact
replay of all 46 events. The result recorded `transport: direct_tcp` and
`direct_lan_verified: true`. SSH handled setup and process control only; no
forwarded connections or automatic tunnel fallback participated. Temporary peers
and the remote bundle were cleaned up. The normal daemon port 42424 is separate
from this disposable test listener; no permanent Linux service was installed.
