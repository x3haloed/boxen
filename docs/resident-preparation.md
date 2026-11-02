# Preparing a resident home

A prepared home does not start inference. A resident can be registered with
`auto_activate: false`, no schedules, and no messaging bindings while its human
prepares a first activation. Discord can be the primary communication surface;
resident GUI controls are not a prerequisite.

## Operator runtime policy

`BOXEN_HOME/runtime-policy.json` is optional. The daemon reads it when accepting a
new run and retains the validated policy in `record.runtime_policy`. Atomic file
replacement changes future runs without a daemon restart; accepted runs and exact
submission retries retain their original policy. Existing records without this
field use the defaults. Invalid policy rejects new acceptance rather than silently
falling back; restoring a valid file allows new work without manual receipts.

```json
{
  "providers": ["deepinfra"],
  "allow_fallbacks": false,
  "max_output_tokens": 8192,
  "max_message_sends": 2
}
```

This initial operator surface is host-wide. Provider slugs become OpenRouter
`only` and `order` constraints; `require_parameters` remains true. Both workload
and summary-helper requests use the captured routing. The output setting applies
to workload exchanges; summary helpers retain a separate 2048-token cap. Requested
routing is not proof of the serving provider: native frames and bounded
`X-Provider-Name`/`X-Generation-Id` response metadata remain in `inference_frame`
events when supplied. No alternate provider or model is retried by Boxen.
See [OpenRouter routing](https://openrouter.ai/docs/guides/routing/provider-selection).

Defaults are no explicit provider restriction, fallbacks enabled, 2048 output
tokens and 16 message attempts per run. Output tokens range from 1 to 131072;
message attempts from 0 to 256; at most 16 bounded provider slugs are accepted.
The policy file is limited to 16 KiB. Model/provider limits can be lower.

The shared resident-tool boundary reserves each outgoing message attempt in the
existing run journal before dispatch. Legacy `boxen_send_message` and generic
`boxen_messages` sends share a budget across builtin, Codex dynamic-tool and
promoted MCP execution. Failed attempts consume a reservation too. An exact
call-ID/input retry reuses its reservation; changed input under that ID rejects.
New calls beyond the limit receive a causal tool error and create one retained
`message_send_limited` notice per run. Other tools and private narration remain
available; a later activation receives a fresh budget. This pauses further new
sends in the affected run, not the resident's future existence. It is an
application-tool limit, not an OS sandbox, a cross-run rate limit or a restriction
on direct network access through Full Access shell tools.

## Durable Linux service

Keep executables outside source checkouts and store releases by content hash,
with a stable `current` symlink. A user systemd service can launch the actual APE
through Bash:

```ini
[Unit]
Description=Boxen daemon
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
ExecStart=/bin/bash %h/.local/share/boxen/current/boxen.com --data-dir %h/.boxen serve --listen 0.0.0.0:42424 --ui 127.0.0.1:42425
Restart=on-failure
RestartSec=3
UMask=0077

[Install]
WantedBy=default.target
```

User lingering allows service startup at boot and survival across logout. LAN
firewall rules must permit the chosen peer TCP port; the HTTP control listener
stays on loopback. Pair using public identities. Credentials remain private files
on the executing host; they are never embedded in the unit or copied implicitly
by pairing. The control URL printed at startup contains a capability, so service
logs are private operator material. Opt-in diagnostic logging stays off until
explicitly enabled through the running daemon API.

For upgrades, inspect all active runs, wait for idle, stage a tested release,
replace `current` atomically and restart the service. Keep the prior executable;
rolling it back does not roll back journals or undo effects. Back up the complete
Boxen home and resident workspace while stopped, or use a consistent filesystem
snapshot. Snapshot archives containing credentials need private storage.

## Migrating from another runtime

Preserve an original archive and integrity manifest before preparing files. An
archive is retrievable evidence, not automatically accepted conversation history.
Boxen has no Watch conversation importer. Historical source speech, identity
notes, runtime framing and later interpretations retain distinct provenance.
Incident-authored files remain preserved with their creation times; copying them
neither endorses their claims nor authorizes replacing the resident's memory.

A first private activation can inspect original sources and author its own durable
account. Later stages can add one explicit Discord binding, manual mail
activations, automatic activation, scheduling and observation streams. Preparing
those stages does not enable them. A model fixture pass is not a migration round
or evidence that a resident has reviewed, accepted or understood its new home.

## Verification

The inference runtime smoke exercises captured wire routing/output settings,
legacy and generic send budgets in one activation, retained limit notices,
continued private narration, producer metadata, restart/replay, fresh activation
budgets and invalid-policy recovery across restart. The Linux rotation includes
this controlled suite. These checks establish runtime behavior, not resident
comprehension or consent to migration.

On November 2, 2026, strict Clippy, 27 Rust tests, native macOS runtime and
platform checks, actual APE runtime/platform checks, and all seventeen Linux
controlled suites passed. The identical APE passed direct LAN streaming/replay
and remote cleanup. A separately supervised Linux installation completed one
real MiMo v2.6 Flash exchange with DeepInfra reported in retained metadata,
without tools; its pairing and completed result survived service restart. No
resident activation or Discord traffic was part of that installation check.
