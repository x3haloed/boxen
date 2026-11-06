# Promote a Codex conversation to a resident

Promotion attaches a durable resident identity and mailbox to an existing ordinary
Boxen Codex conversation, including an imported thread. The canonical Boxen ID,
native Codex thread, working directory, model selection and historical journals
remain in place. Registration starts no model turn and does not acquire an
external writer. This slice adds CLI/API capability, with no GUI changes.

Every activation using the Boxen resident MCP bridge sets server-wide
`default_tools_approval_mode = "approve"`. All tools on `boxen_resident`, including
inbox, correspondence, continuity and scheduling, execute without MCP tool approval
prompts. This applies to existing and future promoted residents on their next
activation; fresh residents use the existing dynamic-tool path without MCP approvals.
Other MCP servers, browser permissions and execution approvals retain their policies.
No global Codex configuration changes. Authenticated activation identity, owner-local
routing, revision checks and stable-ID deduplication remain enforced. Each run retains
`resident_bridge_approval_policy` and matching optional diagnostics. Approval does not
turn private narration into correspondence or prove receipt comprehension.

```sh
boxen resident-promote HOST:PORT PUBLIC_KEY \
  '{"thread_id":"BOXEN_THREAD_ID","name":"My resident","auto_activate":false}'
```

The authenticated peer operation or normal loopback destination envelope is:

```json
{
  "op": "resident_promote",
  "spec": {
    "thread_id": "BOXEN_THREAD_ID",
    "name": "My resident",
    "identity": "Human-authored identity notes, if desired",
    "memory": "Optional initial portable memory",
    "quota_bytes": 65536,
    "auto_activate": false
  }
}
```

`thread_id` is the canonical 64-hex Boxen ID, not the native Codex UUID. `name` is
required; identity and memory default to empty, quota to 64 KiB, and automatic
mail activation to true. An explicit false retains mail without automatically
running it, using the ordinary resident activation policy. No incoming mail is
created from historical conversation text.

## Acceptance and ownership

The thread must belong to the authenticated owner, have an accepted native Codex
session binding, and have no active Boxen run. Unsupported runtimes, conflicting
resident IDs, invalid quotas and conflicting promotion retries are rejected.
The caller cannot change backend, working directory or model in this operation.
The new resident is standalone; no shared definition is attached implicitly.

The resident snapshot is the promotion authority. It retains the immutable
promotion request, original native ID and source Boxen run ID. Repeating the exact
promotion returns the current evolved resident and repairs mailbox registration
if needed; it does not reset memory or start another conversation. No separate
identity or receipt store duplicates the resident/mailbox authorities.

Existing anonymous-thread schedules and observation subscriptions have a distinct
request/response target contract. Enabled ones require an explicit pause or
replacement before promotion; promotion does not silently rewrite their handler
or directions. Resident schedules/subscriptions can then be configured normally.
A paused legacy thread target remains a legacy target; reenabling it does not
convert it into a resident schedule. New anonymous targets for a resident are
rejected.

After promotion, new direct thread starts and steering are rejected. Existing
historical exact-ID submissions can still be reconciled without execution, and
historical run records retain their original accepted identity/provenance.
Current correspondence uses the mailbox contract: receiving, activation and
explicit sending are separate, and assistant output is private narration.

Another Codex client may still own the native writer. Promotion does not release
it. Actual continuation requires that writer to be released by its owner. A
missing/rejected native session or an unexpected resumed native ID fails visibly;
there is no automatic reconstruction or replacement for a promoted conversation.

## Resident tools on an older native session

Codex registers dynamic tools when creating a native session; resume alone does
not add them. Promoted sessions therefore receive a per-activation **MCP bridge**
to the existing daemon-owned resident and observation operations. Fresh residents
keep their existing dynamic-tool path. Both paths use `src/agent_tools.rs`.

The bridge is a private loopback HTTP endpoint with a fresh capability bound to
one accepted activation and resident. The capability reaches only that app-server
child through an environment variable. Per-resume MCP configuration contains the
endpoint and environment-variable reference; Boxen writes no global Codex config,
capability file or credential into its journal or diagnostics.

Before starting inference, Boxen verifies that the resumed native session exposes
the bridge's continuity and messaging tools. The native session's saved developer
instructions are not replaced. Current resident framing and the accepted
identity/memory snapshot accompany the activation as separately labelled runtime
context, distinct from message authors. Historic assistant text remains history,
not newly sent correspondence. Original native tools and continuity remain native.

Bridge calls are retained before dispatch in the run journal, and their results
are retained before returning. Exact repeated call IDs return their recorded
result; conflicting inputs are rejected. A recorded call with no result stays
unknown and is not dispatched again on that retry. Stable message IDs still
identify deliberate retries across different tool calls or activations. Unknown
prior effects do not block a resident's next activation or trigger automatic
historical replay.

The endpoint stops with its activation and rejects effects after terminal status.
Headers are bounded at 16 KiB, requests at 1 MiB, and request reading has a total
two-second deadline. Authentication and method rejections send their HTTP response
before a write-side shutdown, then discard incoming body bytes within the same
deadline and a 1 MiB cap. A stalled rejected client cannot retain the serial
handler indefinitely. The serial handler and existing run limit bound its execution
resources. Structured diagnostic events are opt-in and omit message/memory bodies
by default. Authoritative journals remain active when diagnostics are disabled.
This is application routing under the agreed Full Access model, not OS isolation.

## Verification and current scope

```sh
python3 scripts/resident-promotion-smoke.py target/release/boxen
python3 scripts/thread-import-live.py target/release/boxen \
  --codex /absolute/path/to/codex --model YOUR_AVAILABLE_MODEL --promote
```

The controlled suite uses actual paired daemons, a deterministic native backend
and real HTTP MCP calls. It checks explicit registration without inference,
concurrent exact retries, ownership/quota conflicts, historical replay, protected
native settings, continuity writes, selective replies, private narration, call
deduplication/conflicts, authentication, graceful rejection of buffered and delayed
request bodies, bounded stalled-body recovery, bridge lifetime, daemon restart and native
resume failure without reconstruction. It shares the import regression setup.
The Linux rotation includes it alongside the existing suites.

The opt-in live test consumes model usage and uses only disposable native and
Boxen conversations. Its saved-instruction sentinel is absent from the imported
public history, so recalling it checks native instructions independently of the
snapshot. It also checks an earlier conversation marker, actual resident memory
updates and explicit mail, followed by a no-send activation on the same native
thread. Its evidence remains under ignored `work/thread-import-live/`.

Production agents have not been promoted by these tests. Cross-host native
session transfer, an external desktop participant connector, rollback/demotion,
definition association for promoted residents, and resident GUI UX remain separate
work. Runtime versions must support per-resume MCP configuration and thread-scoped
MCP discovery. The native integration follows the official
[app-server](https://learn.chatgpt.com/docs/app-server) and
[Codex MCP](https://learn.chatgpt.com/docs/extend/mcp) contracts.

### November 2, 2026 evidence

On Windows x86-64, 27 Rust unit tests, strict all-target Clippy, release builds,
the import/promotion controlled suites and the resident-class regression suite
passed. The latter also exercised promotion of an ordinary Boxen Codex thread.
Native `platform-check` and `scripts/platform-smoke.py` passed blocking/wakeup and
CPU checks (about 0.03 seconds CPU over 0.55 seconds wall time). The platform smoke
and class fixture launcher now support Windows without changing the Unix paths.

The actual Codex CLI `0.166.0-alpha.5.1` / `gpt-6-luna` proof passed same-native-ID
promotion, prior marker recall, the saved instruction sentinel, MCP continuity
writes, an attributed explicit reply and a later no-send activation. The global
Codex config file remained byte-for-byte unchanged. Both disposable native test
chats were archived; no production agent was imported or promoted.

The original Windows commit did not claim macOS/APE/Linux checks. No APE artifact was built
in this Windows checkout, and the authorized Linux SSH connection was blocked by
an untrusted host key in this host's SSH configuration. The seventeen-suite Linux
runner includes the promotion suite and its fixture helper for the next verified
portable rotation; this verification did not bypass host-key validation.

### HTTP rejection recovery verification

The subsequent rejection fix passed native macOS ARM64 promotion checks, three
additional consecutive promotion runs, resident-class regression, strict Clippy
and all 27 Rust unit tests. Actual APE promotion and timing/idle-CPU checks passed
on macOS ARM64. The identical APE passed all seventeen controlled Linux suites,
direct LAN streaming/replay and remote cleanup on Debian 13 x86-64. The regression
exercises 401/405 responses with buffered and delayed bodies, a stalled sender,
and successful authenticated requests after the drain deadline. The expanded
checks failed against the preceding binary. This follow-up used controlled
backends; Windows and live model behavior were not reverified for the fix.
