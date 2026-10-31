# Problems to solve

This is a design inventory, not a settled architecture or an implementation checklist. Identity, ownership, segregation, runtime contracts, and failure semantics are early decisions because they constrain the rest.

| Area | Questions |
| --- | --- |
| Identity and ownership | What identifies an agent definition, resident, thread, and run? Who owns their records? Can concurrent runs act on one thread? |
| Placement and portability | Where is an agent defined, where does its durable state live, and where is it executing? What makes a destination eligible? Does a move transfer the next turn, reconstruct a session, or transfer live execution? |
| Credentials | Where are secrets stored, unlocked, and scoped? When may they be shared across machines? Can remote work use a credential without copying it? How do rotation and revocation propagate? |
| Home and work segregation | How are files, memory, credentials, tools, networks, and communication separated? Where are OS accounts or VMs needed? How are crossings granted? |
| Enrollment and connectivity | How do machines pair, authenticate, discover, and reach each other across NAT, firewalls, changing addresses, and sleep? How are machines removed or recovered? |
| Scheduling authority | Who assigns work? What continues offline? How are competing decisions reconciled? What authority is local, domain-wide, or shared? |
| Failure and recovery | How are failed, completed, and unknown outcomes distinguished? What survives crashes and interrupted streams? Which actions can safely retry? |
| State and synchronization | Which records are authoritative? What replicates, to whom, and when? How are offline conflicts resolved? How does Boxen state relate to backend-owned sessions? |
| Workspaces and dependencies | How do workers obtain repositories, uncommitted edits, packages, tools, and local services? How do concurrent agents share or isolate edits? |
| Runtime contracts | Which lifecycle operations are shared? How are steering, approval, cancellation, observation, recovery, and backend-specific capabilities exposed? |
| Built-in runtime | How are inference, tools, history, compaction, interruption, tool-result pairing, and provider failures handled? |
| Multimodal transport | How are large artifacts, live streams, timestamps, and generated media represented and moved? How are native capabilities and explicit conversions negotiated? |
| Mailboxes and coordination | What are the delivery, ordering, acknowledgement, and retention semantics? How do addressing, delegation, cancellation, and loop prevention work? |
| Triggers | How do messages, timers, and streams wake agents? What happens to missed schedules, overlapping runs, bursts, and sleeping machines? |
| Resources and budgets | How are CPU, memory, disk, concurrency, inference spend, and provider limits allocated? How does interactive work compete with background activity? |
| Permissions and approvals | Who grants tool and side-effect authority? Where do remote or unattended approvals appear? How are grants scoped and expired? |
| Extensions and upgrades | How are extensions installed, trusted, isolated, configured, and versioned? How do mixed daemon versions cooperate? What happens to active work during upgrades? |
| Inspection and control | How can people and agents understand running, waiting, blocked, and disconnected work, inspect evidence, intervene, and recover? How do quiet residents remain first-class? |
| Retention and recovery | What survives deletion? How are logs and artifacts cleaned up? How do backup, restore, and export preserve useful state? |
| Compatibility and verification | What qualifies as supported for each OS, runtime, and provider? How are interruption, reconnection, upgrades, and capability differences tested? |

## Candidate first slice

A proposed first proof is to pair two machines, submit an anonymous thread with an attachment, execute remotely, detach the GUI, and reconnect to progress and results. Then exercise the same path with a named resident, mailbox, and recurring trigger.

Cover one managed runtime and one inference-only backend, including a disconnect and daemon restart. This proposal has not yet selected the runtime contracts, storage design, transport, language, or scheduling topology.

## Proposals still requiring decisions

- Separate agent definitions, threads, runs, triggers, and domains.
- Start with one durable scheduling authority per domain.
- Treat initial mobility as placing subsequent execution on another eligible machine.
- Use versioned process protocols for extensions.
- Use typed content and artifact references across runtime boundaries.
- Consider Rust for the resident daemon.

These are starting hypotheses. They must not silently become requirements merely because they were suggested first.
