# Messaging connector constraints

Research date: 2026-11-01. The first implementation is documented in
[messaging](../messaging.md).

## Discord findings

- Message-content access affects native message fields in API responses too.
  Empty text, embeds or attachment fields cannot establish that a human sent an
  empty message. The native message reference identifies which fields were
  available; Boxen labels ambiguous empty input accordingly.
  [Discord message resource](https://github.com/discord/discord-api-docs/blob/main/developers/resources/message.mdx).
- Gateway connections add their own intents, heartbeat, session/resume and
  reconnect requirements. A first HTTP-polling connector keeps these mechanics
  outside resident/runtime logic; a Gateway implementation can replace that
  acquisition loop through the same instance-scoped intake API.
  [Discord Gateway](https://github.com/discord/discord-api-docs/blob/main/developers/events/gateway.mdx).
- Rate limits depend on authenticated bot/account, route and shared buckets.
  Hardcoded throughput guarantees are inappropriate. The first runner respects
  retry durations and exhausted-limit headers conservatively across its account;
  more concurrent bucket scheduling can improve throughput later.
  [Discord rate limits](https://github.com/discord/discord-api-docs/blob/main/developers/topics/rate-limits.mdx).
- `enforce_nonce` checks recent same-author nonces for only a short period.
  It complements the daemon's durable dispatch ledger; it cannot justify retrying
  an uncertain historical send after an arbitrary restart.
  [Create Message fields](https://github.com/discord/discord-api-docs/blob/main/developers/resources/message.mdx#create-message).
- Allowed mentions, native message references and text-size limits are service
  semantics. The bundled runner preserves explicit reply references, disables
  automatic mention parsing and rejects oversized outgoing text. Subject lines
  belong to transports that declare them rather than the common envelope.
  [Discord message resource](https://github.com/discord/discord-api-docs/blob/main/developers/resources/message.mdx).

The external account is a property of an instance, while room IDs and actor IDs
are meaningful inside the relevant provider/account context. A single process or
provider type is not a universal routing namespace. Explicit resident bindings
allow both independent bots and intentionally shared bot identities.

The abstraction is a public message-intake/outbox protocol with a bundled local
transport and a reference Discord addon. The shared durable store owns accepted
facts; native provider code owns service behavior. This keeps provider SDK/transport
assumptions out of the resident's tool loop and permits an addon in another
language without changing Boxen. It preserves one correspondence authority rather
than bridging a second Discord inbox database into Boxen mail.
