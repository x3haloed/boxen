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
dependencies. Native macOS ARM64 and Windows x86_64 desktop execution have been
exercised. The Linux menu was exercised in an X11 session under Xvfb. Windows checks include
the real-daemon client/correspondence tests and opening the native workspace and
resident-recipient composer. This is not a notarized release
or installer. The GUI has its own Cargo manifest/lockfile and never enters the
Cosmopolitan daemon dependency graph.

A running daemon is required. The default attachment is `http://127.0.0.1:42425`
and `~/.boxen/control-token`. `BOXEN_HOME` or `--data-dir PATH` selects another
Boxen home; `--url http://127.0.0.1:PORT` selects another loopback control port.
Capabilities are read from the private file, never command-line arguments or
query strings, and the client rejects non-loopback origins.

The Threads pane includes this machine and already-paired peers discovered
from the daemon. `lan-peer.json`, when present, supplies the development peer's
address. You can also set it with **Machine connections** in the command palette.
The optional `--peer PUBLIC_KEY --address HOST:PORT` overrides one peer's address.
A peer address locates an already-pinned identity; it grants no new trust.

## First interactions

- On macOS, Boxen uses the system menu bar. Windows uses a native menu bar in
  the Boxen window. Linux uses an in-window menu on both X11 and Wayland.
  **File** contains **New Thread** and **Reconnect Daemon**, which is enabled
  when the desktop has yet to connect. **View** contains **Show Work** and
  **Command Palette…**.
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
- **⌘/Ctrl + Shift + P**, **⌘/Ctrl + P**, **F1**, or **View → Command Palette…** opens the palette.
  Typing filters commands and threads; arrows select, Enter opens, Escape closes.
  **Show thread list** restores a closed list; **Reset docking layout** restores
  the sidebar while preserving open conversations and drafts.
- **New Thread** in Threads, File, or the command palette opens a compact dialog.
  **Start thread** uses this machine, the default Codex model, and a working area
  under the executing daemon's data directory at `threads/<thread-id>`. Expand
  **Run settings** in the dialog to choose the machine, directory, runtime, or model.
  When the daemon queues the first run, the dialog closes and the thread opens
  in **Messages**. A rejected submission preserves the draft and settings.
  When queue status is uncertain, the catalog checks for the same request ID and
  **Retry same request** submits that exact request again.
  On an idle conversation, **Run settings** can change the directory or model
  for the next send. Choosing another machine moves the idle conversation,
  including its retained history and attachments. The draft follows the conversation
  and remains ready to send. The conversation composer sends a follow-up, or steering while
  a run is active. **⌘/Ctrl + Enter**
  submits; cancellation is a separate control.

The presentation is deliberately simple: selectable text, exposed thinking,
tool activity/output, status and incidents. Rich Markdown, rendered attachments,
backend input forms, resident management, scheduling and observation-source controls are
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

## Project work

The **Work** toolbar button and **Show work** command open a single dockable project
panel. It lists retained work items and presents current status/reason, PR link,
candidate commit, context/technical review results and expandable evidence. Later
no-action observations retain previous PR deliveries. Stale source observations
and unavailable replies remain explicit. PR delivery does not claim merge or
deployment, and the panel does not contact Azure to certify current PR state.

Each item has its own saved feedback draft. **Ask a question** addresses the context
resident and shows explicit replies alongside the original feedback and in Mail.
**Return for correction** retains the displayed candidate and original operator
text, then uses the service's fresh interpretation, same-coder assignment and
review workflow. Returns become available for terminal workflow work; active work
can still receive questions. The service validates the displayed basis rather than
silently applying feedback to an unseen newer candidate.

The optional service defaults to the sibling `.boxen-work-items` home;
`BOXEN_WORK_ITEMS_HOME` selects another local installation. Its configured daemon
home/origin must match the desktop attachment. Service/SQLite/HTTP work stays off
the render thread. The service library is linked only into the independent native
desktop package, outside the portable daemon graph. The local JSON operator CLI
is documented in [the workflow contract](../addons/work-items/WORKFLOW.md).

The panel is added once on upgrade; closing it remains respected. Drafts survive
selection, layout and restart. Exact unresolved submissions are saved before I/O,
never automatically resent after restart, and offer an explicit same-ID retry.
Observation alone does not activate agents or perform workflow transitions.

Native client/service release helpers remap source and dependency paths. The
Windows executables also use a generic embedded PDB reference. Distributable
artifacts still require a final personal-path/configuration scan.

## Mail correspondence

The **Mail** panel is docked on the left alongside Threads. It aggregates
mail addressed to stable human desktop clients across configured machines;
agent-to-agent traffic and private execution narration are not inbox messages.
The client ID is the local daemon's stable public machine identity, so its address
`client:MACHINE:MACHINE` is discoverable by any authorized custom client without
GUI storage. Mail remains host-local under existing owner scoping; the desktop
polls each execution host's client inbox rather than moving mail into a second
store. **Copy mailbox address** exposes that address.

Single-click previews message details; double-click keeps them open. Message and
thread previews share one global slot across all docking surfaces. Drafting or
dragging pins a preview. Details include subject, sender, original text and a reply
composer. **New message** opens a pinned editor with an agent-instance selector
(machine and short ID distinguish names), subject and body. The palette offers
**Show mail** and **New message**. Layout reset preserves open mail
editors and drafts, and existing saved layouts gain the new sidebar once.

Sending queues ordinary Boxen mail, with explicit reply provenance when replying.
Acceptance is not a promise of a reply or proof of reading. No assistant bubble
or agent activation is fabricated. Replies, selective replies and no-reply
completion retain the daemon's mailbox contract. Subjects are bounded to 256
bytes and bodies to 32 KiB. A confirmed send shows its retained receipt/text;
there is no Sent-folder or activity transcript in this iteration.

The sidebar draft list reopens unfinished new messages after their editors close.
Drafts, local read markers and exact unresolved requests survive layout changes
and desktop restart. The exact request ID/spec is saved before transmission.
Polling reconciles acceptance by that ID but never sends or repeats work. An
uncertain request retains its text and offers an explicit same-ID retry; closing
its editor does not discard it or own delivery lifetime. Original correspondence
and receipts remain in the daemon journal. GUI-local read markers do not claim
that an agent read or understood a message.

Inbox pages refresh off the render thread, with independent machine cursors and
explicit per-machine errors. The agent selector refreshes every ten seconds and
retains only display metadata. The extended desktop smoke test exercises the
real native client through paired native and actual APE daemons: discovery,
exact-ID sends/retries, resident-authored incoming mail, reply provenance, and
completion without a reply. Workspace tests cover cross-kind preview replacement,
pinning, serialization after layout, and existing pointer-event docking behavior.

Native macOS correspondence verification used a disposable paired-daemon fixture:
single-click preview replacement, double-click pinning, editing pinning, recipient
selection, reply and new-message acceptance, retained reply provenance, closing
and reopening an unsent draft, moving message details into a separate dock group,
and restoring the saved layout on application restart. These native UI checks
remain separate from controlled backend tests and do not send production mail.

## Agents panel

Agents is a compact, independently dockable panel above the default
Threads/Mail group. Existing saved layouts gain it once; closing it
stays respected across restarts, and **Show agents** in the command palette
restores it. Existing editors, drafts and previews retain their layout/state.

Standalone residents appear as individual rows. Instantiated residents appear
under an expandable definition/class row with instance counts. Class grouping
uses the machine and definition ID, not a name comparison; independent classes
with identical names remain separate. The latest definition name labels a class,
without applying definition changes to its existing instances. Missing definition
metadata falls back to the definition ID. Each resident identifies its machine
in the status line; hover text supplies exact identity, model and source run.

Green indicates ready/no observed activation or an idle resident whose latest
activation completed or was cancelled. Blue indicates starting/running/cancelling.
Red indicates the latest activation failed or was interrupted. Gray indicates
unavailable/unconfirmed status, including a disconnected machine; cached healthy
state is never shown as current after a failed catalog or resident refresh. A
class with a failed member remains red while its summary also reports active
members. Cancellation is distinct from failure. Imported history snapshots are
not treated as activations. These are observed execution states, not an inference
availability probe or proof of model attention.

The panel reuses background catalog polling (about two seconds) and resident
refresh (about ten seconds). Definition metadata and available inference-health
records enrich grouping/failure descriptions. Older daemons without health
projection retain generic interrupted/failed labels. It issues only authenticated
read operations; viewing/expanding a resident never activates it or delivers
private narration. Resident activity is described below; agent controls remain separate work.

Desktop tests cover grouping/isolation, imported-history exclusion, state
transitions, disconnected caches, saved docking migration, and real paired-daemon
resident/definition discovery with running and credential-blocked fixtures.

## Resident activity

Single-clicking a resident or an expanded class instance opens a conversation
preview titled with that agent's name; double-clicking pins it. The conversation
is a chronological projection of mail between the human client and that instance,
with sent messages on the right, received messages on the left and a simple message
composer. Subjects remain available in the separate Mail editor; chat bubbles
omit them and new chat messages use a generic conversation subject. No private
activity is presented as delivered correspondence. The Activity shortcut beside
a resident's name opens the same conversation tab in Activity. Opening either
view does not activate the resident. Drafting pins a conversation, and exact
uncertain sends retain the same
explicit retry behavior as other mail editors. Sent history is read through the
existing paginated resident inbox API only for open conversations, filtered to this
network's stable human-client sender identities. Each host's stable human inbox
and this desktop's explicit machine/client inbox are paginated independently.
This also exposes correspondence retained before another desktop attached;
reading history does not move mail, repeat delivery or activate the agent.
Arbitrary client IDs, including automation clients, remain separate. Original
addresses and journals remain authoritative; the desktop combines their views.
These reads work with network-capable daemons without a daemon upgrade.
Existing Mail panels and drafts remain accessible.

Resident conversations, ordinary threads and Mail share one preview across docking
groups, floating surfaces and machines. Dragging a conversation tab or choosing
**Keep open** pins it. Class headers continue to expand their instances rather
than open a combined identity. Saved layouts migrate separate resident Activity
tabs into the corresponding conversations.

Activity observes the resident instance's canonical conversation. Its private
narration, backend-exposed thinking, tools/results, accepted activation input,
status, incidents and context-cut metadata are grouped by run. Historical imports
are labeled separately from execution and retain source-role labels. This is an
audit view: private narration is not a delivered reply, and viewing, replaying or
loading older activity never submits mail, starts inference or repeats effects.
The conversation composer and legacy Mail editors send ordinary Boxen mail.

A view initially observes the latest ten runs. **Load earlier activity** extends
its retained-history window by ten. Its earliest loaded run remains anchored when
new runs arrive. Run snapshots are compact catalog metadata; original event
journals supply all displayed activity through the existing authenticated replay
stream. Stream cursors, per-viewer generations, completed-item replacement and
reconnection retain the ordinary thread-view guarantees. Live runs are observed
before historical replay under the existing sixteen-stream bound. Closing or
replacing an activity preview detaches its observers without cancelling execution.

Tests cover real pointer preview/pinning, global preview replacement across
floating surfaces, anchored history windows, private/imported role projections,
and streamed resident activation replay against native and actual APE daemons.
Remote observation setup allows twelve seconds for the loopback controller to
resolve/connect/authenticate its peer before HTTP headers arrive. A delayed-setup
regression verifies that a six-second setup still delivers retained activity;
observation workers retain their finite body leases and cursor-based renewal.

## Network discovery and outgoing mail

The existing workspace refreshes daemon membership about every ten seconds and uses daemon-retained member endpoints. New members appear without rebuilding GUI state; removed or changed destinations detach their existing observers. Fresh mail submissions use the local daemon's durable network outbox. Existing receipt text distinguishes saved/queued mail from destination mailbox acceptance; background receipt checks do not clear newer drafts or resubmit messages. Network-origin provenance identifies the sender's machine for reply selection. No new panels or controls are added.

## Conversation views and attachments

The Agents and Threads panels retain separate lists. Both open a conversation
with **Messages** and **Activity** in the same tab. The selected view belongs
to the conversation; a draft belongs to the composer, independently of that
view. **Show in Activity** moves to the corresponding message. Scrolling away
from the end stops following new items; **Jump to latest** resumes following.

Messages uses left and right placement for authorship. Resident Messages
contains explicit mailbox correspondence; Activity places those same messages
alongside private activations and narration. Anonymous Messages contains user
requests, accepted updates, and assistant text. Activity adds ordered runtime
events. Tool output and results stay with their call, while failed calls and
pending input remain visible without opening routine activity. There is no
standing success banner.

Enter sends from the focused composer; Shift+Enter adds a line. A resident
message can be sent during an activation. Reply explicitly selects an earlier
message and leaves a removable quote in the draft. A pending resident send
retains its own exact request while the next draft can be written. Anonymous
updates stay tied to the intended run; if it ends, a separate action prepares
the draft for a follow-up. No uncertain update is replayed automatically.

The composer accepts up to eight original files, up to 32 MiB each, through the
file picker or a drop on the editor. Text is optional with a file. File chips
open details, a bounded image or text preview, and removal for drafts or
**Save as** for sent files. **Files** locates the message that carried a file.
The source pathname is not retained in the message. Drafts and exact retries
use the staged bytes, even if the file at the source pathname changes.

The daemon checks retained originals before accepting an anonymous request or
a network mailbox envelope. An offline resident's mail can queue on the source
machine; the destination receives and checks the bytes before accepting that
mail. Codex and the built-in runtime receive filenames, sizes, and paths on the
executing host. A model only receives the contents through an explicit file read;
Boxen does not automatically inject text, pixels, or archive contents. See
[the attachment protocol](attachments.md).
