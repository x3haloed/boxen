# External observation adapters

These optional Python 3 examples use the versioned process protocol documented in
[the implementation contract](../../docs/events-runtime.md). Python is an example
adapter dependency; the Boxen executable does not require it. Commands run on the
source's host with its normal Full access and environment. No credentials or
production subscriptions are installed by these examples.

- `usgs.py`: read-only polling of the public USGS past-hour GeoJSON feed. It emits
  at most 32 reports/revisions per call, with a bounded `(updated, id)` cursor.
  This feed is a moving window, not a complete earthquake archive. Recommended
  polling is at least 60 seconds; delivery cadence and waking are independent.
  [USGS documents the format and update cadence](https://earthquake.usgs.gov/earthquakes/feed/v1.0/geojson.php).
- `grid-game.py /absolute/private/state.json`: a real persisted three-by-three
  game. The center is blocked; the goal is `(2,2)`. `look` returns current state;
  `move` receives `direction` and the current `revision`. Stale/blocked moves
  return rejection plus fresh observations. Declare those query/action tools in
  the source definition. This deliberately small game proves the extension
  boundary; it is not a performance or general gameplay benchmark.

Example source registration through an existing paired peer:

```sh
boxen events HOST:PORT PUBLIC_KEY '{"action":"create_source","spec":{"id":"SOURCE_ID","name":"Earthquakes","command":["python3","/absolute/boxen/examples/adapters/usgs.py"],"cwd":"/absolute/working/directory","interval_ms":60000,"timeout_ms":5000,"mode":"ordered"}}'
boxen events HOST:PORT PUBLIC_KEY '{"action":"subscribe","spec":{"id":"SUBSCRIPTION_ID","source":"SOURCE_ID","target":{"type":"resident","id":"RESIDENT_ID"},"interval_ms":900000,"waking":false,"lifetime":"persistent"}}'
boxen events HOST:PORT PUBLIC_KEY '{"action":"list"}'
```

IDs are 64 lowercase hexadecimal characters, for example generated with
`python3 -c 'import secrets; print(secrets.token_hex(32))'`. Paths and commands are
executed on the selected host. First collection may wait for the source's next
poll. An optional top-level `at_ms` on registration sets its first poll, including
a short future delay while subscriptions are being configured.

Agent-created subscriptions use an activity lifetime with a one-hour default
expiry. Persistent interests are selected explicitly. Neither routine turns nor
heartbeats renew an activity. The tools expose subscription IDs, source
versions, health, and action declarations; an adapter can be created or replaced
outside Boxen without adding a Rust game class.
