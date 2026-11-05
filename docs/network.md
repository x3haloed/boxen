# One network of machines

Pairing admits a machine to the user's Boxen network. Members have full operator
access to each other's agents and threads, including ones created before pairing.
Admission to one member propagates to the other reachable members. This replaces
the earlier interpretation of each pair as an isolated authorization relationship.
A daemon/data directory participates in one network; selectively sharing a host
with a guest is outside this first implementation.

## Membership and endpoints

Machine public keys remain the authenticated identities. Existing `peers/KEY.json`
records now carry membership generations, removal records and endpoint revisions.
Noise still proves possession of the corresponding private key; an endpoint or
label alone never authorizes a connection. Public keys and small address records
propagate; private keys and inference/connector credentials do not.

Initial admission still uses reciprocal `trust` through an already authenticated
bootstrap channel. The optional address makes the peer available to background
synchronization without opening a GUI first:

```sh
boxen trust OTHER_PUBLIC_KEY "Other machine" --address example-host:42424
boxen serve --listen 0.0.0.0:42424 --advertise example-host:42424 --name "Workstation"
```

A successful direct connection also remembers a previously unknown endpoint.
Membership exchange learns the sender's reachable IP/listen port when needed.
`--advertise` supplies an explicit LAN endpoint; a subsequent explicit `trust
--address` updates a member's endpoint. Local desktop address fields remain
compatible bootstrap inputs. Reachability, identity and membership are distinct.
All participating daemons need this network-capable version; an older paired
implementation does not gain network-wide authorization or mail transport.

One background worker per daemon exchanges bounded snapshots over the existing
Noise transport. It sleeps between passes and backs off unavailable peers, with
coalesced diagnostic failure counts. It does not maintain a persistent connection
to every member, copy workload journals or require an always-on coordinator.
Limits are 128 retained membership records (including removals), 4096 resident
address entries per host, and 1 MiB per cached host directory. Only a directly
authenticated host publishes that host's resident directory. A cached location
continues to support outgoing queue acceptance while its host is unreachable.
Traffic uses direct source-to-destination connections; membership introductions
do not relay workload traffic or bypass host firewalls.
An unknown or ambiguously advertised resident location rejects acceptance rather
than inventing a route. Resident relocation remains outside this slice.

Members are equally trusted administrators and can admit or remove machines.
Merging two previously separate paired groups joins their membership. Membership
is eventually consistent, not a consensus log: disconnected members learn changes
when a surviving connection becomes available. Concurrent records converge by
generation, removal precedence, endpoint revision and deterministic tie-breaking.
An endpoint update alone is not a new admission. A damaged member record stays unavailable and preserved without denying unrelated healthy peers; network status and transition diagnostics expose the issue.

`boxen untrust KEY` now records a network removal rather than deleting a local
allowlist file. The removal propagates and survives restart; stale snapshots do
not undo it. Connections and retained-event watches recheck current membership.
An unreachable machine cannot learn revocation instantaneously, and removing a
member does not stop its already accepted workloads or erase its local records.
Explicit reciprocal pairing readmits an identity with a newer admission generation;
ordinary restart and endpoint refresh do not. A removed installation that learned
its own removal needs explicit local pairing as well as a current member's
readmission. A partitioned group with no connection to current members cannot
receive membership updates.

## Operator access and provenance

The authenticated machine-operator boundary aggregates host-local catalogs and
resolves existing objects to their original continuity namespace. Stored `owner`
keys remain original creation/continuity provenance; new access does not rewrite
resident IDs, mailbox addresses, native sessions or historical run ownership.
Follow-up work and activation continue in that original namespace. Revision
checks, exact-ID conflict checks and native backend pins remain unchanged.

This boundary covers discovery, history/replay, context, resident updates and
activation, thread controls, definitions, schedules, observation configuration,
connector administration and inference recovery. Agent-facing tools still act as
the attached resident; sharing operator access does not turn an agent's mailbox
read or send into impersonation of another resident. Local connector-instance
capabilities retain their instance/action allowlist and cannot become a general
operator or forwarding capability. Pairing remains full execution access, not
OS sandbox containment.

The existing loopback authenticated operation envelope supports:

```json
{"op":"network","args":{"action":"status"}}
{"op":"network","args":{"action":"admit","key":"PUBLIC_KEY","label":"Other machine","address":"example-host:42424"}}
{"op":"network","args":{"action":"remove","key":"PUBLIC_KEY"}}
```

`GET /api/status` includes member endpoints in `peers`; custom clients can discover
them independently of desktop storage. The desktop refreshes membership and uses
those endpoints for the existing machine, agent and thread projections. The
ordinary CLI also accepts `boxen request HOST:PORT PUBLIC_KEY JSON` for an
authenticated operation without a GUI.

## Durable cross-machine mail

An agent's stable resident ID is its Boxen address. The compact host directory
locates it. Human client addresses retain their explicit machine and client IDs;
joining a network does not merge separate human client identities. Operators can
query a retained client namespace with `client_inbox` and an explicit `machine`.

The existing correspondence journal owns outgoing acceptance, destination mailbox
intake and delivery receipts. There is no second network inbox database. A source
accepts an immutable message and its destination route in one journal record
before acknowledging it. It retains at most 256 pending network sends, dispatching
bounded batches of eight with per-message retry deadlines. Unknown location,
full pending storage or failed retention reports nonacceptance.

Source-side `delivery.status: queued` means **saved on the accepting machine,
awaiting destination acknowledgment**. `accepted` with `accepted_on` means the
destination retained it in the recipient's mailbox. Neither means activation,
attention, comprehension or reply. `messaging status` exposes the current receipt,
attempt count, next retry and bounded failure cause. `mailbox_status` includes the
pending network count. Source outgoing copies are excluded from inbox projection.

The receiver validates the authenticated source, local recipient and exact retained
message. Repeated delivery after lost acknowledgment returns the original intake
without duplicate mailbox entries or another presentation. Changed input under
an accepted ID rejects. New inbox acceptance uses normal optional activation;
messages arriving during an activation wait for a later batch. Private narration
never becomes mail automatically. Network replies use the same explicit send path.

Exact retries retain the original route even when the directory changes. Restart
does not resubmit model work or historical tool effects. A removed destination
retains queued mail and an unavailable-route cause; it cannot receive that mail
while removal is in force. Explicit readmission can make that retained route
available again. A destination storage failure leaves the source message queued
rather than claiming delivery.

The desktop submits newly composed mail to its local daemon for durable outgoing
acceptance, so closing the GUI or losing destination connectivity does not own
execution or delivery lifetime. Its existing receipt text distinguishes queued
mail and polls accepted-message status without resending. Saved unresolved older
requests retain their exact original destination/request for explicit retry.

## Verification

`scripts/network-smoke.py` runs three actual daemons with only adjacent bootstrap
pairs, then checks indirect membership, a new direct connection, preexisting
host-local resident discovery/update, cross-namespace history replay, offline mail,
source restart, destination storage failure, conflicting retry rejection, a deliberately lost real Noise acknowledgment, exact intake and
explicit replies, resident-to-resident cross-host relay and no-reply completion. It fills the pending outbox and checks that removal prevents
dispatch without discarding accepted queued mail. It also checks retained removal and rejection of an old member.
The controlled Codex backend supplies model/tool behavior; this is not live model
evidence. The same script runs against native and actual APE artifacts and is part
of the Linux rotation. Platform checks include notification/blocking behavior and
idle CPU. Existing runtime, messaging, scheduler, context and desktop suites retain
their effects/provenance tests with shared machine-operator access expectations;
identity-scoped runtime and connector capabilities remain distinct.
