# LAN pairing prototype

The current slice distributes a single executable to another LAN machine, runs listeners on both hosts, establishes durable mutual trust, and exchanges authenticated, encrypted echo messages directly between them. SSH is used only to bootstrap development deployment and exchange public identities through an already trusted channel.

There is no remote command execution, agent runtime, workload scheduler, automatic discovery, NAT traversal, mailbox delivery queue, or persistent background peer link yet. Each exchange creates a fresh authenticated connection. A paired peer is a trusted identity, not proof that its host is currently reachable.

## Build and packaging

`cargo build --release` produces a self-contained native binary at `target/release/boxen` on Unix. `bash scripts/build-ape.sh` produces `target/cosmo/release/boxen.com`, using pinned cosmo-build 6.0.2 and cosmo-compat 6.0.7. The APE contains ARM64 and x86-64 code. Do not strip intermediate executables before APE fusion.

The build helper verifies the Cosmopolitan compiler archive's SHA-256. Build-tool caches default to ignored `work/cosmo`; set `COSMO_HOME` to reuse a cache elsewhere. If the default compiler download is unavailable, `COSMO_COSMOCC_URL` can select an official mirror while retaining checksum verification. The verified archive for compiler 4.0.2 is also available from the official Cosmopolitan GitHub release.

On macOS and Linux invoke the APE using `bash boxen.com ...`; do not assume Finder launch or direct kernel execution. Consumers need no Cargo, Rust, Python, or Node installation to run the executable. Python and SSH are used only by development helpers.

APE build support is not a portability certification. This slice has been exercised on two macOS ARM64 hosts. Linux, Windows, x86-64 execution, signing/notarization, and fresh-machine launch/security prompts have not been verified for Boxen.

## Pair manually

Examples below use the native binary. Substitute `bash /path/to/boxen.com` for the portable artifact.

1. Run `boxen identity` on each host and exchange the public keys through a trusted channel, such as authenticated SSH. Each installation creates a stable machine keypair.
2. On each host, run `boxen trust OTHER_PUBLIC_KEY "Other box"`.
3. Run `boxen serve` on each host. Defaults are `0.0.0.0:42424` for peers and `127.0.0.1:42425` for local controls. Use `--listen` and `--ui` to override them.
4. Run `boxen ping HOST:42424 OTHER_PUBLIC_KEY "Hello"`, or open the local control URL, select the trusted peer, enter its LAN address, and click **Send test message**.

Both sides must trust each other. The initiator also pins the selected server identity before sending application content. Labels are local display names, not network addresses or identity authorities.

`boxen peers` lists trusted keys. `boxen untrust PUBLIC_KEY` revokes trust without restarting the daemon. Trust entries are immutable; remove and re-add one to change its label.

## Development deployment over SSH

```sh
bash scripts/build-ape.sh
python3 scripts/pair-lan.py chad@machine.lan
```

The helper requires SSH key authentication and unused peer/control ports on both hosts. It copies the binary to `~/.boxen/bin/` on the destination, exchanges public keys, records mutual trust, starts both daemons, and verifies direct exchanges in both directions. Use `--binary`, `--local-address`, `--port`, and `--ui-port` as needed. The remote probe uses `lsof`; this helper targets macOS/Linux development hosts, not Windows installation.

The helper leaves daemons running independently of the SSH session. It records `daemon.pid`, private `daemon.log`, and local `lan-peer.json` under the data directory. It installs no login or boot service. Inspect the recorded PID and process command before stopping a daemon; a PID file can become stale. Stop an existing installation before rerunning the helper. Partial setup failures can leave trust entries or a daemon behind; the helper is not a transactional installer.

The local daemon uses the built artifact at its existing path. Rebuilding that artifact does not upgrade an already running process; stop and restart it deliberately. The remote host needs no source checkout or build tools.

## State and access

The default data directory is `~/.boxen`. Override it with `BOXEN_HOME` or `--data-dir PATH`. It contains private machine identity material in `identity.json` and public trust records in `peers/`. Do not commit, distribute, or copy the private identity to another machine. A new machine gets its own identity.

On Unix, Boxen creates private directories with mode 0700 and state files with mode 0600. State is plaintext on disk under the user's account; OS keychain integration and encrypted storage are not implemented. Windows filesystem permissions have not been verified.

The local page uses a per-process bearer capability supplied in the printed URL fragment. It removes the fragment from the visible URL and retains the capability in tab session storage. The API requires authentication and rejects unexpected Host headers; the listener must bind to IPv4 loopback. Opening a fresh tab requires the printed capability URL. Treat logs containing that URL as private.

## Protocol and operational bounds

The peer protocol uses the Snow implementation of `Noise_XX_25519_ChaChaPoly_BLAKE2s` over TCP, with the `boxen-lan-v1` prologue. Both sides prove possession of their persistent private keys; the application then enforces its public-key allowlist. Trust is rechecked before processing a message. Messages and replies are encrypted and authenticated by Noise.

Frames use a two-byte big-endian length bounded by 65,535 bytes; application text is limited to 4,096 bytes. Connections have read/write timeouts, connection attempts have per-address timeouts, and inbound peer handlers are capped at 32. The local HTTP surface is sequential and bounded in request size. These limits are prototype resource bounds, not comprehensive denial-of-service protection.

Echo replies identify the authenticated peer and return the exact sent message. Failed or unreachable exchanges report failure without claiming delivery or durable queuing. Echoes themselves are not retained as messages; only successful exchange metadata is printed. Message logs, workloads, and their recovery contracts are future slices.

## Verification

Verified on October 31, 2026:

- Rust tests cover mutual-trust rejection, expected-server identity pinning, retained identity and trust after state reopening, live revocation, malformed keys, self-pair rejection, and oversized messages.
- Native and APE process smoke tests run two real daemons and exercise exchanges in both directions, embedded controls, unauthenticated API rejection, foreign Host rejection, unreachable peers, daemon restart with retained pairing, and live revocation.
- The identical APE file was copied to a second macOS ARM64 machine. Matching SHA-256 values confirmed the deployed bytes; direct authenticated LAN exchanges succeeded in both directions.
- The remote daemon was stopped and restarted, then direct exchanges succeeded in both directions using retained identities and trust.
- The actual local control page was opened and used to send a message to the remote machine; its authenticated reply was visibly rendered.
- Formatting, unit tests, and Clippy with warnings denied passed.

The proven path is explicit user input → selected trusted identity → authenticated connection → remote trust check → encrypted echo → verified local result. The next frontier is a durable remote workload lifecycle; the current echo path does not establish execution, event retention, or job recovery.
