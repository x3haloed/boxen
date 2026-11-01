# Platform testing rotation

Runtime and packaging changes are checked on the development Mac (native and
portable builds) and the authorized Linux test host, `chad@resident-host.lan`.
The Mac mini remains available for additional macOS LAN/deployment checks.

Build the same portable file with the optional transport probe enabled:

```sh
COSMO_HOME="$PWD/work/cosmo" cargo build --locked --release --features ape,inference-probe
python3 scripts/test-linux.py
```

The runner defaults to `chad@resident-host.lan`; another SSH target can be supplied
as its first argument. `--binary PATH` selects another built APE, and `--output
PATH` selects the local evidence directory. The host needs passwordless SSH,
Python 3, Bash, tar and OpenSSL, plus SSH TCP forwarding for the cross-host proof. Rust, Cargo, Node, an installed Boxen service,
Codex, and model credentials are not prerequisites. The runner checks for the
optional probe commands before uploading rather than silently skipping tests.

Only the binary, selected test scripts, and controlled Codex fixture are copied
to a private `/tmp/boxen-linux-*` directory. The worker records the actual OS,
architecture, kernel, distribution, Python version, binary SHA-256, suite exit
statuses and timings. These ten existing suites run against the uploaded file:

- Blocking, notification and idle CPU checks (`platform`).
- Pairing, authenticated controls and revocation (`smoke`).
- Workload streaming, control, ownership and restart (`workload`).
- TCP partition/reconnect, retained replay and uncertainty (`recovery`).
- Resident identity, continuity and recovery (`resident`).
- Selective delivery, no-send completion and batch recovery (`mailbox`).
- Hot logging filters, file/stream sinks and failure behavior (`logging`).
- Built-in inference, actual shell effects and communication (`inference-runtime`).
- Native media wire bytes, quotas and snapshot recovery (`media`).
- SSE/native-state projection and TLS rejection (`inference-probe`).

A failed suite stops the rotation, retains its local log/report, and returns a
nonzero status. A suite has a 180-second deadline. This is a controlled-provider
rotation: it does not copy OpenRouter keys or make paid model requests.

After the Linux suites pass, temporary peers on the development machine and
Linux exchange authenticated messages in both directions through loopback-bound SSH TCP forwards across the LAN.
The development machine then submits a controlled Codex-fixture workload to
Linux, observes command output before completion, and checks contiguous event
sequences and exact replay. Only public pairing information crosses the SSH
control stream; temporary GUI control tokens are not printed or retained in
logs. Test listeners and forwards use temporary ports, bound to loopback. Production daemons, identities and
pairing files are not changed. The temporary peers stop and the remote bundle is
removed; cleanup status is included in the result.

Evidence is saved locally under `work/linux/UTC_TIMESTAMP/{suites.log,result.json}`.
The JSON includes Linux suite results and the cross-host result, explicitly labeled `ssh_forwarded_tcp`. A matching
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

This adds Linux x86-64 APE evidence. It does not certify every Linux distribution,
Linux ARM64, Windows, macOS x86-64, native Linux Rust builds, installed real Codex
on Linux, or live inference providers on Linux. Model perception tests previously
run on macOS remain separate from these exact-wire Linux fixture checks.

The first direct cross-host probe also found that inbound temporary TCP listeners
on Linux were unreachable from the Mac. A plain Python listener on `0.0.0.0`
timed out too, so this is not established as a Boxen transport defect. The
rotation leaves host network policy alone and uses explicit SSH forwards for its
cross-host proof. Direct Mac-to-Linux LAN reachability remains unverified; a
successful tunneled result does not claim otherwise.

The completed rotation used APE SHA-256
`551ee61602e0f7a8f04d443bd6711ccefb644b166ce0937b67b36e13c384df51`.
Both-direction messages and the Linux fixture workload passed over the explicit
SSH forwards. The Mac observed tool output before completion and exactly replayed
all 46 retained events. The report marked remote cleanup successful. The
production Boxen installation and the Linux host's network policy were unchanged.
