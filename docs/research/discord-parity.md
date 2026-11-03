# Discord parity with Watch

Watch source reviewed: `src/discord.ts`, `src/tools/discord.ts`, and the shared
message/reaction tools. This is a behavior inventory, not a requirement to copy
Watch's singleton account, private state stores, or error-to-context feed.

| Behavior | Watch | Boxen parity work |
| --- | --- | --- |
| Text/replies | Explicit send, splitting, first-part reply reference | Implemented; durable multipart outcomes |
| Accounts | One bridge per harness | Preserve instance/account isolation and deliberate sharing |
| Permission | Native access plus DM whitelist/configuration | Implemented restrictive operator grants, separate from resident attention |
| Attention | DMs, mentions, replies; watched channels/threads; muted scopes | Implemented independent durable attention within permission |
| History | Chronological centered/latest/older/newer windows | Implemented bounded native context query through the connector |
| Reactions | Explicit reaction action; passive reactions on own posts | Implemented action receipts and bounded passive observation, not incoming mail |
| Media | Incoming attachment metadata; outgoing local/URL files | Implemented retained local/HTTP(S) snapshots with explicit upload bounds |
| Gateway | Live messages/DMs, reactions, reconnect and presence | Implemented optional native transport, bounded buffers and legible resume gaps |
| Presence | Ready/queued/busy/offline mapped to Discord | Implemented opt-in aggregation; shared accounts do not imply one identity |
| Recovery | In-process bridge, reconnect, rate limiting | Keep durable intake/claims, unknown effects and component isolation |

## Shared contract

An operator's instance policy determines which native surfaces and actions are
permitted. Each associated resident's attention determines which permitted
messages are interesting. Both predicates must pass for intake fanout. Outgoing
operations are independent of attention. Watching never grants permission.

Existing immutable bindings seed exact-conversation permissions and watched
interest. Existing installations therefore keep their current behavior. New
broader grants do not implicitly subscribe a resident to everything. Directed
attention defaults are DMs, mentions and replies; a deliberate watched `all`
scope can select all permitted ordinary messages. Explicit deny grants override
allows. Other bots require an allow grant with `accept_bots`; own-account posts
remain excluded.

Scopes are `conversation`, `space` (Discord guild), `parent` (a thread's parent),
`user`, `direct`, and `all`. Actions are distinct `read`, `receive`, `send`,
`react`, `files`, and `presence`. User scope identifies the incoming author or
an outgoing DM correspondent; it is not arbitrary authority over a shared room.
Policy starts from no grants beyond existing bindings and is operator-controlled.
Attention is resident-controlled with revision checks. Policy changes can tighten
permissions without resetting identity, history, mailbox, or interests.

Queue acceptance is not service authorization. When native metadata is needed,
an outgoing request can be provisionally queued; dispatch rechecks current policy
against connector-supplied native context before any service effect. This context
comes from the trusted runner, not model-authored arguments. Instance checks are
application routing boundaries, not an OS sandbox over Full access processes.

## Construction boundaries

The existing daemon connector configuration owns grants and interests. The
existing correspondence journal owns intake, service requests, claims and effects;
run journals own tool results and presentation. Observation streams own passive
reaction buffers and loss. No parallel provider inbox, receipt store, or runtime
is introduced. The official GUI is outside this work.

The selected shape extends the portable Rust runner and public authenticated
addon protocol. A credential-bearing tool loop inside each agent would duplicate
routing and make remote accounts depend on workload placement. Copying Watch's
Node/discord.js process would lose the single-executable deployment path.

Verification includes native and actual APE processes, instance/owner isolation,
permission versus attention, restart and revision behavior, exact outgoing
effects, interrupted effects, bounded input and passive observations. Linux uses
the standard regression rotation. Controlled Discord/provider fixtures are
separate from explicitly authorized live bot/channel evidence. New capabilities
are not automatically enabled on production accounts, nor do they migrate an
existing runner to another machine.

## Primary protocol references

- [Discord message API](https://docs.discord.com/developers/resources/message)
- [Discord Gateway](https://docs.discord.com/developers/events/gateway)
- [Discord Gateway events](https://docs.discord.com/developers/events/gateway-events)
- [Discord rate limits](https://docs.discord.com/developers/topics/rate-limits)

## Verification boundaries

Controlled checks exercise paired real daemons, real WebSocket framing, restart
resume, invalid sessions, permission/attention separation, scoped native actions,
private passive reactions, local/URL snapshots, exact multipart bytes and idle CPU.
Live checks separately exercise Discord Gateway/REST with an explicitly configured
test account and temporary resident: native history, reaction, attachment posting
and same-session resume. Those service checks do not claim live human intake.
The live mode that waits for human correspondence remains available separately.

This covers the reviewed Watch behaviors with bounded upload limits and without
copying its singleton bridge. Native message edits/deletes, rich components,
creating DM channels by user lookup, sharding and cross-host account leases remain
outside the implementation. Polling remains the compatible default; registration
never silently switches an existing account to Gateway or broadens its grants.
