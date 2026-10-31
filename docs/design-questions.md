# Problems to solve

This is an inventory of unresolved design work, not a settled architecture or an implementation checklist. [Agreed decisions](decisions.md) constrain these questions. Identity, ownership, segregation, runtime contracts, and failure semantics are early decisions because they constrain the rest.

| Area | Questions |
| --- | --- |
| Identity and ownership | What identifies an agent definition, resident, thread, and run? Who owns their records? How is single execution ownership enforced for a canonical conversation? How are clone inheritance and optional live sharing represented? |
| Placement and portability | How are state location, execution location, and placement preferences represented? What makes a destination eligible? How is required continuity synchronized before activation, and source execution stopped before handoff? |
| Managed-session mobility | Can a runtime's complete native session move, with its opaque state and required artifacts, under the selected runtime/provider versions? When does a canonical resident require pinned execution rather than context reconstruction? See [the portability investigation](research/managed-session-portability.md); native fidelity is unverified. |
| Credentials | Where are secrets stored, unlocked, and scoped? When may they be shared across machines? Can remote work use a credential without copying it? How do rotation and revocation propagate? |
| Home and work segregation | How are files, memory, credentials, tools, networks, and communication separated? Where are OS accounts or VMs needed? How are crossings granted? |
| Enrollment and connectivity | How do machines pair, authenticate, discover, and reach each other across NAT, firewalls, changing addresses, and sleep? How are machines removed or recovered? |
| Scheduling authority | Who assigns work? What continues offline? How are competing decisions reconciled? What authority is local, domain-wide, or shared? |
| Failure and recovery | How are failed, completed, and unknown outcomes distinguished? What survives crashes and interrupted streams? Which actions can safely retry? |
| State and synchronization | Which records are authoritative? How are portable history and backend sessions related? How are shared identity and memory versioned and propagated promptly? How are offline conflicts resolved? |
| Conversation reintegration | How do branches return experiences or results to a canonical conversation? What records integration, and what happens to branch history afterward? |
| Workspaces and dependencies | How do workers obtain repositories, uncommitted edits, packages, tools, and local services? How do concurrent agents share or isolate edits? |
| Runtime contracts | Which lifecycle operations are shared? How are steering, approval, cancellation, observation, recovery, and backend-specific capabilities exposed? |
| Built-in runtime | How are inference, tools, history, compaction, interruption, tool-result pairing, and provider failures handled? |
| Multimodal transport | How are large artifacts, live streams, timestamps, and generated media represented and moved? How are native capabilities and explicit conversions negotiated? |
| Mailboxes and coordination | What are ordering, acknowledgement, and retention semantics? How do durable acceptance and delivery statuses propagate? How do addressing, delegation, cancellation, and loop prevention work? |
| Triggers | How do messages, timers, and streams wake agents? What happens to missed schedules, overlapping runs, bursts, and sleeping machines? |
| Resources and budgets | How are CPU, memory, disk, concurrency, inference spend, and provider limits allocated? How does interactive work compete with background activity? |
| Permissions and approvals | How are movement and model/runtime selection grants represented, scoped, and expired? Where do backend approval requests appear? How could execution access evolve beyond the initial Full access policy? |
| Extensions and upgrades | How are extensions installed, trusted, isolated, configured, and versioned? How do mixed daemon versions cooperate? What happens to active work during upgrades? |
| Inspection and control | How can people and agents understand running, waiting, blocked, and disconnected work, inspect evidence, intervene, and recover? How do quiet residents remain first-class? |
| Retention and recovery | How is the configurable portable-file quota enforced and exposed? How are conversation history and attachments budgeted separately? What survives deletion? How do cleanup, backup, restore, and export preserve useful state? |
| Compatibility and verification | What qualifies as supported for each OS, runtime, and provider? How are interruption, reconnection, upgrades, and capability differences tested? |

## First implementation slice

Start an anonymous workload remotely through Codex app-server, observe it through completion, cancel it, and detach and reconnect without stopping execution or losing recorded observations. Use explicit placement and destination-local files. See [the acceptance criteria](decisions.md#first-implementation-slice).

The built-in inference runtime follows to test the shared contract. Resident continuity, portable state, mobility, configurable conversation structure, and reintegration remain beta requirements. The initial [LAN transport](p2p.md) and [remote Codex workload contract](workloads.md) use Rust and Noise over direct TCP. Events stream immediately and replay by sequence; unfinished runs become uncertain after a daemon restart. General runtime contracts, durable storage retention, wide-area connectivity, scheduling topology, and richer workload recovery remain open.

## Proposals still requiring decisions

- Separate agent definitions, threads, runs, triggers, and domains.
- Start with one durable scheduling authority per domain.
- Use versioned process protocols for extensions.
- Use typed content and artifact references across runtime boundaries.

These are starting hypotheses. They must not silently become requirements merely because they were suggested first.
