# Agreed design decisions

These decisions describe the intended product and the first implementation slice. Later beta requirements remain binding even when early implementations support only simpler cases. Agent storage and cross-host scheduling topology remain undecided. The initial LAN implementation uses Rust and direct TCP with Noise authentication and encryption; this does not settle discovery or wide-area connectivity.

## Packaging and GUI ownership

Prefer one portable executable with no installed application runtime or complicated setup. The initial implementation follows the Cosmopolitan APE approach: one file containing ARM64 and x86-64 builds. A launchable GUI and daemon may eventually use separate binaries if that improves the experience. Native platform builds remain available from the same source.

The user will primarily direct GUI design. Early controls should provide simple clickable access to capabilities without establishing a detailed product layout. The intended official GUI is a Rust desktop app, not a hosted web application. It may have a separate binary from the daemon. The daemon must remain friendly to alternative GUIs, including user-generated clients: authenticated discovery, submission, replay, observation, and control belong to its documented API and cannot depend on the official GUI. The current plain HTML surface embedded in the daemon and served only on loopback is provisional.

The first native desktop slice uses one interchangeable docking tree for side panels and conversation tabs, with nested splits, resizing and a command palette. Anonymous conversations use VS Code-style preview/open semantics: at most one preview across the workspace; single-click replaces it, double-click keeps a tab open. Drafting or moving a tab also keeps it open. The GUI remains a separate Rust executable attaching to the existing authenticated daemon API. See [desktop](desktop.md) and the [framework investigation](research/desktop-framework.md).

Commit and push coherent, verified implementation slices as part of the development workflow.

## Daemon diagnostics

Instrument the daemon throughout development, as required by [AGENTS.md](../AGENTS.md). Structured diagnostic logging is off by default, with runtime enable/disable, authenticated remote shell subscriptions, file sinks, and producer-side filters. Keep the disabled path cheap and log fields lazy. Bound consumer queues and report loss rather than blocking workloads behind log I/O. Diagnostic controls must not require a restart or affect authoritative workload journals. See [logging](logging.md) for the implemented contract.

## Resident identity and conversations

A resident can use a single canonical conversation that continues indefinitely. Runtime sessions and context windows may change without creating a different resident or ending that conversation.

Conversation structure is configurable per agent:

- One canonical conversation.
- Multiple conversations sharing an identity, with no requirement to reconcile them.
- Branch conversations with an explicit method of reintegration into the whole or a canonical conversation.

Threads of one continuing resident share its durable identity and memory. Synchronizing memory is distinct from reintegrating conversations; receiving a fact does not mean a branch's experience has been integrated. Reintegration semantics remain to be designed.

Clones default to inheriting identity and an initial memory snapshot, then developing independent mutable continuity. Live shared memory is an explicit option. A reusable worker identity does not imply eventual reintegration of its instances.

[Resident definitions](resident-definitions.md) now supply immutable versioned templates. Definition changes reach existing instances only through an explicit manual application, with selected replacement fields and a resident revision check. Instances retain independent identity, memory, mailbox and canonical conversation. Workload completion does not dispose of an instance. Manual archive/delete may be added separately; this slice adds neither automatic cleanup nor archive/delete operations. Shared mutable memory and parallel conversations of one continuing identity remain separate future work.

## Portable continuity

Conversation history must be transportable independently of backend-owned sessions. Preserve backend session state where supported without claiming that reconstruction transfers hidden backend state losslessly.

An agent has a special portable persistence layer for identity, memory, configuration, conversation history, and deliberately retained agent-owned files. This state follows the agent. Machine-local files and execution environments do not automatically follow it.

At activation or relocation, the agent must understand:

- Which machine it inhabits.
- Which files, tools, services, and execution environment are available there.
- Where its durable persistence layer is accessible.
- What changed with the execution location.

Before execution switches to a destination, required continuity state must already be available there. This applies to user-forced and agent-initiated moves, and to the activation of branches and clones. A single canonical conversation has one active execution owner at a time; destination readiness alone does not prove that the previous host stopped.

Updates to shared identity and memory must propagate promptly to connected participants. Required revisions must be available before destination activation. Revision tracking, conflict handling, offline behavior, and measurable propagation targets remain open.

## Deliberate durable storage

Arbitrary agent-owned portable files are allowed within an enforced configurable total quota. Agents must be able to inspect usage and understand quota failures. The limit encourages intentional retention and prevents every resident from imposing an unbounded synchronization burden.

Scratch storage stays local by default. Conversation history and attachments need separate budgeting, retention, and replication rules so their growth cannot silently defeat the portable-storage limit. Exact limits and accounting rules remain open.

## Authority and execution access

Movement policy is configurable per agent. Support both explicit user-directed movement and standing permission for autonomous movement among eligible, granted machines.

Model and runtime choice are separate configurable permissions. Agents may be allowed to choose among configured backends within their grants and budgets; users may also pin these choices.

Early versions use full execution access for managed and built-in runtimes. Codex app-server threads start with Full access. Sandboxing is deferred and can be revisited as usable settings and enforcement become available. Movement grants and credential distribution remain separate from execution access on a host.

## Unreachable hosts and message delivery

Distinguish accepting a message from delivering it to an agent. If a receiving daemon durably queues a message for an unreachable host, report that it is saved at the receiving location and awaiting delivery. If it cannot save the message, report that failure.

Record delivery attempts and subsequent status changes. Show the condition to the sender; do not silently discard messages or present queued messages as delivered. Describe a host as unreachable unless its offline state is actually known.

Loss of contact is not evidence that execution failed or completed. Safe relocation, retry, and ownership handling during network partitions still require design.

## First implementation slice

From Machine A, start an anonymous workload on Machine B, observe its activity, and retrieve its outcome.

Acceptance criteria:

- Explicitly select the destination, backend, and working directory.
- Execute through Codex app-server as the first managed runtime, using Full access.
- Observe progress, tool activity, any approval requests, and final output.
- Cancel the workload.
- Close and reopen the client without stopping execution.
- Disconnect and reconnect without losing already-recorded events or mistaking lost contact for completion.
- Retain durable thread and run identifiers, execution location, and recoverable observations.
- Stream backend-exposed thinking summaries, agent messages, tool activity, and command output as they occur. Remote execution should feel local; completion-only delivery or periodic polling is insufficient.
- Replay missed events in sequence after reconnection without duplicating displayed content or coupling execution lifetime to the viewer.

Use destination-local files and explicit placement initially. Resident identity, shared memory, automatic scheduling, and relocation are not prerequisites for this slice. File transfer is not required to prove it.

The next slice adds the first resident: a named identity, one canonical conversation, and a quota-managed identity/memory store on an explicitly selected host. The [first built-in inference runtime](inference.md) now verifies text/tool execution and mailbox communication through OpenRouter. Native image, audio, video, and PDF reading is now implemented for compatible OpenRouter models/routes; broader provider coverage remains planned. Movement follows; parallel conversations, clones, and reintegration remain part of the beta requirements.

The initial [LAN pairing prerequisite](p2p.md) now carries [remote Codex workloads with live event streaming](workloads.md). The executing daemon owns each run and its retained event sequence. A client can detach and reconnect; a daemon restart marks unfinished execution uncertain rather than retrying it automatically. Broader recovery, storage retention, production enrollment UX, and wide-area connectivity remain open. The [built-in inference backend](inference.md) now adds OpenRouter text/tool workloads using the same run, observation, control, and mailbox authorities.

## Recovery favors availability

Prefer continued availability with a durable, understandable incident record over stopping simply because a previous outcome is uncertain. Do not require manual acknowledgment, proof collection, or state-certification ceremonies before the next turn. Keep the original interrupted outcome and side-effect uncertainty intact; a new message continues the conversation without automatically repeating old work.

An explicitly unresumable managed-backend session may be replaced beneath the same canonical Boxen conversation. Supply saved identity/memory and retained history with clear reconstruction limits. A malformed resident record is isolated and reported rather than taking healthy residents and anonymous workloads offline. This principle does not authorize new machine access, copying credentials, or silently abandoning history.

## Communication follows identity attachment

Identity-bearing agents use mailbox communication and receive their own mailbox by default. This is not a selectable alternative to a conversational resident mode. Receiving information, activation, and sending are independent: an activation can read a batch of incoming messages, occur without an incoming message, or end without sending a reply. Final assistant output is private execution narration, observable by authorized viewers but not implicitly delivered to original senders or rooms. Replies and other outgoing messages are explicit tool operations. Conversation topology (canonical thread, parallel conversations, or branches) remains a separate identity/continuity choice.

Identity-less ephemeral threads use ordinary request/response semantics. They are workload control channels rather than long-lived agents. Agents can address them by thread ID through dedicated thread request/response tools with Codex-like conventions; those tools are distinct from tools that send to agent mailboxes or rooms. The conventional response remains part of the ephemeral request contract, preserving the reliability of ordinary remote Codex work.

The [mailbox implementation](mailboxes.md) now enforces these contracts for host-local communication. Existing residents are registered automatically, direct resident turn submissions are rejected, and activation is independent of message receipt. Automatic activation on new mail is enabled by default and configurable per resident. Mail arriving during execution waits for the next activation. Rooms, offline cross-host routing, and completion-driven resident activation remain later work.

## Scheduling and heartbeat pattern

An ordinary scheduler supports one-time future events and recurring jobs of different kinds. A heartbeat is a typical recurring-job pattern: for example, an hourly system ping to an agent carrying directions crafted and retained when the heartbeat job was defined. The scheduler determines when an event is due; the event handler determines its effect. A separate heartbeat-only scheduling subsystem is not the intended design. The [host-local scheduler](scheduling.md) now implements one-time and anchored fixed-interval jobs for resident activation and ordinary identity-less thread requests. Both use existing execution and communication contracts. Overdue recurring ticks coalesce, active targets wait, and accepted work is reconciled without replay after restart. Cross-host ownership/movement and calendar recurrence remain planned.

## Observation streams and external adapters

The [agreed events design](events.md) separates source collection, bounded buffering, presentation, and activation. Subscriptions support scheduled batches with optional waking, ordered event history or fresh current-state snapshots, and explicit provenance/loss. Agent-created subscriptions default to activity scope; persistent interests and time-limited subscriptions are explicit options. Turn completion does not end an activity, and unrelated heartbeats do not silently renew it.

Connector health is separately inspectable. Meaningful failure/recovery/suspension notices are compact and rate-limited, including flapping; repeated errors update counters rather than fill model context. Retry and resource containment belong to software and cannot depend on the agent repairing or unsubscribing. Stop/expiry survives outages and reconnects. Interactive feedback includes queries/actions and fresh observations at supported runtime boundaries. The preferred extension boundary is a versioned external adapter process and authenticated publishing API, allowing game integrations without changing Boxen. The [first host-local JSON slice](events-runtime.md) now implements these contracts through external adapters, the existing scheduler, bounded pending storage, and both runtime bridges. Native stream-media projection, persistent adapter processes, cross-machine source mobility, and broader backend delivery support remain planned.

## Causal agent-facing language

Boxen-authored runtime context, tool descriptions, errors, and recovery notices describe mechanisms and consequences rather than issuing imperatives. Identity/personality and directions can come from the agent's own notes or its human; Boxen's substrate supplies affordances and accurate operating context. Human- and agent-authored text remains intact. A narrow imperative exception can be considered when observed failures show causal wording is insufficient, with its evidence and scope documented. Messaging conventions do not receive a blanket exception in advance. [AGENTS.md](../AGENTS.md) maintains this requirement during implementation.

## Compaction can preserve original voice without summarization

Compaction/context management must optionally work without a summarization pass. Some residents require access to the original wording of their past rather than having it substituted by another voice. A model-written account must not overwrite original history or be presented as an original utterance. A selected summary-free policy must not silently invoke summarization as its overflow fallback.

Archival retention, current input selection, exact retrieval, and model recall are distinct properties. A finite window cannot contain an indefinitely growing verbatim conversation. The selection/recall policy and defaults remain to be designed; the [compaction research](research/compaction.md) compares mechanisms and evidence. The built-in runtime now implements selectable [summary and verbatim policies](context.md); exact model-token/window accounting and broader migration requirements remain open.

The [resident compaction co-design](compaction.md) develops that policy with the resident: exact source/neighbor recall, explicit coverage, masking older tool bodies first, chronological source-bundle pinning, and optional attributed drafts reviewed against source. The resident accepts a preselected summary-free availability fallback with visible shortages; review is not a prerequisite for continuing. Mechanical coverage is verifiable, while semantic importance and behavioral continuity remain evaluated claims. The document includes a proposed boundary record, re-entry framing, and migration tests; the separate [implementation contract](context.md) identifies what now exists. Neither document authorizes migration. Summary-free operation remains available independently of reviewed drafting.

Managed runtimes own their existing native modality tools and model-input chain. Boxen integrates supported attachment transport and observation rather than implementing a second image-processing agent loop inside Codex or Claude Code. The built-in inference runtime now provides native file reading for text, image, audio, video, and PDF inputs through `boxen_read`; model/provider support is required. See [the implemented bounds and verification](inference.md#native-file-reading).

## Pluggable messaging and connector instances

Messaging uses a shared intake/send/status contract. Boxen mail is bundled; external providers attach through configured instances rather than one global bot/account per provider. Each instance owns its expected native account and credential reference/configuration, while explicit conversation/resident bindings govern participation. One resident can use multiple accounts, and multiple residents can share an account deliberately. Native authors, conversations and message IDs remain attributable within the instance. Receiving, activation and explicit sending remain independent; anonymous workloads retain their separate request/response contract.

The first [messaging slice](messaging.md) uses the existing correspondence journal for common intake, presentation links, external dispatch claims and honest outcomes. External addons use a public API; the bundled Discord runner is a separate foreground subcommand, not a resident-runtime dependency. Unknown external effects are never automatically replayed. The original mailbox APIs and storage field names remain for existing clients/native session histories; compatibility removal requires an explicit versioned migration.

Discord permission is operator policy; resident attention independently selects permitted input. Defaults preserve exact bound conversations and deny additional surfaces/actions. Broad receiving grants do not implicitly watch ordinary traffic. Native history, reactions and retained local/URL uploads use declared connector operations and the existing correspondence receipts. Optional Gateway intake retains accepted sequence, exposes resume gaps and publishes own-post reactions as passive private observations. Presence requires a separate grant and aggregates shared accounts. See [Discord parity](research/discord-parity.md).


## Built-in resident model changes

An operator can change a built-in inference resident’s model with a revision-checked resident update. Future activations use the saved model while accepted runs, source attribution, identity, memory, mailbox and canonical history remain intact. Backend and working-directory changes, managed-session model switching, and anonymous-thread model switching remain outside this operation. Definition application retains its existing post-execution pins.
