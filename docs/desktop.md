# Desktop workspace (first slice)

Boxen now has a native Rust desktop client. It attaches to the existing daemon;
execution, pairing, ownership and retained history still belong to the documented
[client API](client-api.md). The daemon's portable executable is unchanged.
The embedded web client remains available for capabilities outside this slice.

Settings → Tool approvals provides independent automatic Computer Use and Browser
Use toggles for each connected executing machine. Both default to on. The daemon
owns their persisted values and applies changes to subsequent permission requests
in active conversations. Settings requests run off the render thread; failed
reads or writes remain visible with a retry control.

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

- macOS and Windows use native menus. Linux uses the in-window menu.
  **File** offers **New Thread** and **Reconnect Daemon**. The toolbar opens the
  finder and tool-approval settings and shows the machines in the network.
- The Watchlist offers Threads, Agents and Mail. Threads groups anonymous
  conversations by machine. Identity attachment in any run excludes a
  conversation from Threads. Machine filters and text search stay local.
- A single click opens one replaceable peek. Double-click or **Open on grid**
  places the conversation. Selecting a visible conversation focuses its
  existing pane. Drafts belong to their conversation.
- The workspace has two rows and adds columns as width permits. A new pane
  can take an empty column. When columns are occupied, choosing a snap region
  splits only the affected pane. Shift with a placement shortcut replaces
  the selected pane. A pane can change span or move into available space.
  Closing leaves a hole. **Tidy** previews an explicit arrangement.
- Narrowing the window parks panes that no longer fit. Widening restores their
  homes. Placing a parked pane elsewhere replaces its remembered home. Parked
  panes, placed panes, and attached drafts survive restart. The desktop
  migrates previously pinned docking tabs into grid homes once. A peek is
  transient.
- **⌘/Ctrl + K**, **⌘/Ctrl + Shift + P**, or **F1** opens the finder for threads
  and commands. The finder also opens project work and new message editors.
- **New Thread** in Threads, File, or the command palette opens a compact dialog.
  **Start thread** uses this machine, the default Codex model, and a working area
  under the executing daemon's data directory at `threads/<thread-id>`. Expand
  **Run settings** in the dialog to choose the machine, directory, runtime, or model.
  The Runtime dropdown lists the configured runtimes reported by the selected
  machine. Changing machines refreshes the choices before a new thread can start.
  The Codex, Antigravity, and Claude Code model dropdowns load from the selected machine and
  include **Default model**. OpenRouter accepts a model ID as text and uses the
  destination's default when left blank.
  When the daemon queues the first run, the dialog closes and the thread opens
  in **Messages**. A rejected submission preserves the draft and settings.
  When queue status is uncertain, the catalog checks for the same request ID and
  **Retry same request** submits that exact request again.
  On an idle conversation, **Run settings** can change the directory or model
  for the next send. **Choose a model** uses the same runtime-specific dropdown
  for Codex, Antigravity, and Claude Code. Choosing another machine moves the idle conversation once
  the destination reports its runtime configured,
  including its retained history and attachments. The draft follows the conversation
  and remains ready to send. The composer sends ordinary follow-ups between runs.
  During a run it offers steering when the runtime supports it. **⌘/Ctrl + Enter**
  submits; cancellation is a separate control.

Pending backend requests appear in Messages and Activity with their original
question and response controls. Pending command approvals sit above history in
both views. The full command is scrollable and copyable, with the backend's
offered decisions below the details. **Approve once** applies to the pending
command. Session and policy options have separate labels when offered.
MCP permission requests offer Approve, Decline and Cancel and show the server
and supplied parameter labels. Nonempty forms expose their schema and a JSON
values editor.
Other backend requests remain visible with a JSON response editor. Replies use
the existing authenticated control path after an explicit response action.
Replay preserves pending requests. An answered or backend-resolved request loses
its active controls.

The expandable **Execution context** detail shows the latest run's machine,
account, session, privileges, working directory and observed desktop-session
availability. Waiting runs display their retained reason, including a missing
configured login session. These values come from run records and replayed
execution events. The [host execution policy](workloads.md#execution-account-and-operating-system-session)
controls selection independently of the desktop.

The presentation is deliberately simple: selectable text, exposed thinking,
tool activity/output, status and incidents. Rich Markdown, rendered attachments,
schema-specific form widgets, resident management, scheduling and observation-source controls are
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

Messages reuses the measured layout of offscreen completed runs once their
terminal sequence has been replayed. The scroll area keeps their space and
draws them when they come into view or are targeted by an anchor. Changes in
width, display scale, or the latest-reply control cause remeasurement. Activity
and runs still receiving events use their existing render paths.

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
A rejected steering send refreshes the addressed run. A terminal snapshot puts
the composer through its usual ended-run transition, with the draft available
for an explicit follow-up.
When Antigravity reports that its prompt ended and a native background command
remains open, the composer offers ordinary **Send** as soon as the daemon
reports that the same session can take a fresh prompt. Late command updates
remain visible on the run that started the command.
The GUI does not store credentials, copy model keys, or depend on browser storage.

Thread panes project context measurements from retained runtime events. Codex
uses its last exchange token count and reported model window. The 85% mark is
a Boxen warning line. A Codex compaction completion appears as a context row and
invalidates the earlier gauge until a new sample arrives. Antigravity uses its
reported session token counts. OpenRouter shows selected bytes against the
budget available after reservations. Missing samples and incomplete replay show
an unknown gauge.

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
The boundary test clicks MCP permission responses and a one-time approval for
a long command through the desktop renderer, sends the decisions across the
paired transport, and checks the backend's exact response and resolved state.
A short-pane pointer test covers both conversation views, full command copying,
and removal of the decision controls after replay of the answer. Projection
tests also cover numeric request ID zero, repeated replay and backend-side
resolution.

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

For larger histories, `python3 scripts/desktop-history-bench.py --runs 800`
loads synthetic journals into an isolated daemon, opens the thread through
the desktop observer, and drives the real conversation panel with egui input.
The report separates catalog time, time to project and display the latest
response, full lossless replay, and p95 CPU frame time during replay and settled
typing. It also exercises an earlier-message anchor, a view switch, and a
resize before jumping to the latest response. Pass `--binary` to exercise an
actual portable daemon. Frame times include layout and tessellation and exclude
OS input dispatch, GPU presentation, and native window compositing.

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
