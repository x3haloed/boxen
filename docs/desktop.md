# Desktop workspace (first slice)

Boxen now has a native Rust desktop client. It attaches to the existing daemon;
execution, pairing, ownership and retained history still belong to the documented
[client API](client-api.md). The daemon's portable executable is unchanged.
The embedded web client remains available for capabilities outside this slice.

## Build and launch

```sh
python3 scripts/build-desktop.py
```

On macOS this produces `work/Boxen.app`, a locally ad-hoc-signed application:

```sh
open work/Boxen.app
```

On other platforms the script builds `desktop/target/release/boxen-desktop`
(`.exe` on Windows). It requires the platform's native graphics/windowing build
dependencies. macOS ARM64 is the currently exercised desktop platform; Linux and
Windows desktop execution are not yet verified. This is not a notarized release
or installer. The GUI has its own Cargo manifest/lockfile and never enters the
Cosmopolitan daemon dependency graph.

A running daemon is required. The default attachment is `http://127.0.0.1:42425`
and `~/.boxen/control-token`. `BOXEN_HOME` or `--data-dir PATH` selects another
Boxen home; `--url http://127.0.0.1:PORT` selects another loopback control port.
Capabilities are read from the private file, never command-line arguments or
query strings, and the client rejects non-loopback origins.

The Threads pane includes this machine and already-paired peers discovered
from the daemon. `lan-peer.json`, when present, supplies the development peer's
address; otherwise **Commands → Machine connections** sets its address. The
optional `--peer PUBLIC_KEY --address HOST:PORT` overrides one peer's address.
A peer address locates an already-pinned identity; it grants no new trust.

## First interactions

- The thread list contains anonymous conversations only, grouped by canonical
  Boxen thread ID. An identity attachment in any run excludes the whole thread.
- Machines appear as collapsible groups inside the Threads pane. The small funnel
  button beside **Filter threads…** opens machine checkboxes and **All**. Unchecking
  a machine hides its group; exactly one checked machine displays a plain thread
  list. No checked machines displays an empty state. Machine visibility is saved
  across launches and does not close tabs or stop observing open conversations.
  The text filter searches threads within the visible machines.
- A single click previews a thread in an italic tab. Another single click replaces
  that preview, wherever it has been docked. There is at most one preview across
  all machines, groups and floating surfaces.
- Double-clicking a list entry or tab keeps it open. Opening from the palette,
  drafting a message, or dragging a tab also keeps it open. Selecting an already
  open thread focuses it without creating another copy.
- Every panel, including the thread list, participates in the same docking tree.
  Tabs reorder and move between groups; edge drops create nested splits. Split
  dividers resize. Right-clicking a tab provides explicit split commands and
  **Eject** for a floating panel. Floating panels currently remain inside the
  application window, rather than becoming separate OS windows.
- **⌘/Ctrl + Shift + P**, **⌘/Ctrl + P**, **F1**, or **Commands…** opens the palette.
  Typing filters commands and threads; arrows select, Enter opens, Escape closes.
  **Show thread list** restores a closed list; **Reset docking layout** restores
  the sidebar while preserving open conversations and drafts.
- **New thread** starts an anonymous request on the machine selected in its
  dialog, with an explicit destination-local working directory and runtime. The conversation
  composer sends a follow-up, or steering while a run is active. **⌘/Ctrl + Enter**
  submits; cancellation is a separate control.

The presentation is deliberately simple: selectable text, exposed thinking,
tool activity/output, status and incidents. Rich Markdown, rendered attachments,
backend input forms, residents, scheduling and observation-source controls are
not part of this desktop slice. The temporary web client still exposes those
implemented daemon controls. Framework choice does not settle future visual UX.

## Execution and recovery

Discovery, catalogs, submission and observation run off the rendering thread.
Catalogs refresh every two seconds, including runs submitted by other clients.
Selected open conversations replay retained runs and then stream live output.
Events apply once by machine/run/sequence; completed items replace their deltas.
Gaps stop that observation and reconnect from the last applied sequence. Actual
transport failures remain visible and retry after backoff; silence does not imply
completion. Stale events from a replaced viewer generation are ignored.

Observation uses a bounded lossless channel and at most 16 current streams.
Finite eight-second stream leases bound detached readers, renewing immediately
from the last losslessly published sequence. Routine renewal is not presented as
an outage. Closing a pane/window only detaches its viewer; accepted work continues.

Workspace layout, per-thread drafts and unresolved submission IDs/specs live in
`BOXEN_HOME/desktop.json` (owner-only creation on Unix). They are client state,
not execution authority. A new submission's exact ID/spec is saved before network
transmission. Acceptance discovered by response or catalog reconciles that ID;
an uncertain submission offers an explicit same-ID retry. Restart does not
resubmit work automatically. Steering/cancellation are sent once and are not
idempotent retries; uncertain steering retains the draft and an incident notice.
The GUI does not store credentials, copy model keys, or depend on browser storage.

## Verification

```sh
cargo fmt --manifest-path desktop/Cargo.toml --check
cargo clippy --locked --manifest-path desktop/Cargo.toml --all-targets -- -D warnings
cargo test --locked --manifest-path desktop/Cargo.toml
python3 scripts/desktop-smoke.py
python3 scripts/desktop-smoke.py --binary target/cosmo/release/boxen.com
# Disposable workspace for native UI testing, cleaned on Ctrl-C:
python3 scripts/desktop-smoke.py --interactive
```

The unit suite covers cross-surface preview replacement, pinning, peer-scoped
thread identity, resident exclusion, sequence deduplication, gaps and completed
item replacement. Pointer-event tests drive actual docking press/move/release
frames through tab splitting, reordering, divider resizing and moving the list to a bottom dock.

The smoke runner creates private paired real daemons and uses the desktop's
actual HTTP client through authenticated local HTTP and encrypted peer transport.
It checks thinking and incremental command output before completion, detach and
cursor replay, repeated transparent stream renewal, final response and exact-ID
retry. Tests shorten the observation lease to 500 ms so several renewals occur
inside one controlled backend workload. The backend is `tests/fake-codex.py`;
these checks do not claim real-model inference evidence.

Native UI checks additionally exercise preview/pin, palette search/Enter,
message submission with live remote activity, moving conversation and list panes
via tab menus, saved workspace restoration, machine accordion collapse, checkbox
filtering (including All/none), single-machine plain lists, and thread search.
OS-level drag automation did not produce a drag on this host; pointer-event docking tests supply that interaction
coverage, while actual native tab-menu moves verify the resulting layout.

A macOS ARM64 idle sample with two conversation panes and two discovered machines
used 0.10 CPU seconds over 10.034 wall seconds (about 1% of one core), with
84.8 MiB resident memory. This is a prototype baseline for a small controlled
history, not a large-history performance claim. The shipped native window also
loaded an existing Mac Mini conversation through the running daemon without
submitting a production workload.
