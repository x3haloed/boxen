# Reasoning effort

An anonymous thread or a resident can save a reasoning effort for future runs.
This works with Codex and with the built-in OpenRouter runtime. A setting change
starts no work. A run captures the setting when Boxen accepts it; editing the
setting while a run is active affects only later requests or activations. History,
resident identity, mailbox, and native Codex session are retained.

```sh
boxen start HOST:PORT PUBLIC_KEY CWD MODEL 'Review this change' --reasoning-effort high
boxen resident-create HOST:PORT PUBLIC_KEY 'Reviewer' CWD MODEL --backend openrouter --reasoning-effort high

boxen thread-reasoning HOST:PORT PUBLIC_KEY THREAD_ID
boxen thread-reasoning HOST:PORT PUBLIC_KEY THREAD_ID VERSION medium

boxen resident-get HOST:PORT PUBLIC_KEY RESIDENT_ID
boxen resident-update HOST:PORT PUBLIC_KEY RESIDENT_ID REVISION --reasoning-effort medium
```

The first `thread-reasoning` command returns the setting and its opaque
`version`; the second saves a new setting against that version. A stale version
following another edit or accepted request requires a fresh read. For residents,
the read returns the resident `revision`. The value `default` restores the backend default. Existing conversations with no setting keep their backend behavior.
Anonymous follow-ups omit `--reasoning-effort` and inherit the saved setting.

The authenticated operation API accepts `reasoning_effort` in the initial
`start.spec` and in `resident_create.spec`. To edit a resident, send
`resident_update` with the level and `expected_revision` in its patch. For an
anonymous thread, send `{"op":"thread_reasoning","id":"THREAD_ID"}` to read or add
`"patch":{"expected_version":"VERSION","reasoning_effort":"medium"}` to write.
The executing host stores anonymous changes in the conversation journal and
carries them with retained history during a move. The accepted run record's
`reasoning_effort` is the captured request, not a measurement of model effort.
A supported turn uses the same captured setting for all its workload exchanges.

- **Codex:** Boxen submits the explicit value as `turn/start.effort`. Use an
  effort advertised for the model by the executing Codex app-server's
  `model/list` (`supportedReasoningEfforts`). The selection applies to the
  next turn even when resuming the same native thread. Reset resolves the host’s effective Codex configuration, or the model’s
  advertised default if the configuration leaves effort unset. Codex retains a
  previous turn’s effort on resume, so Boxen sends the resolved default explicitly.
  See [Codex App Server](https://learn.chatgpt.com/docs/app-server).
- **OpenRouter:** Boxen sends `reasoning.effort` with `none`, `minimal`,
  `low`, `medium`, `high`, `xhigh`, or `max`; default omits `reasoning`.
  This vocabulary is normalized by OpenRouter. Individual models and routes
  can translate, clamp, or reject values. OpenRouter’s model catalog can
  advertise `reasoning.supported_efforts`, `default_effort`, and whether
  reasoning is mandatory. That describes the model at the gateway; endpoint
  routing can still translate an advertised level to a provider’s native
  control. Boxen
  preserves strict parameter routing and sends the requested level without
  an automatic downgrade. OpenRouter may still translate it. A provider
  rejection is visible in the run; a later activation can use a different
  setting. Separate context summary requests keep their own defaults.
  See [OpenRouter Reasoning Tokens](https://openrouter.ai/docs/guides/best-practices/reasoning-tokens)
  and [Provider Routing](https://openrouter.ai/docs/guides/routing/provider-selection).

This control is available through the CLI and API. The desktop has no reasoning
picker in this slice.
