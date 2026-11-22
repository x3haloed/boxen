# Antigravity runtime

Boxen runs the official Antigravity CLI as a native ACP subprocess on the executing machine. By default it looks for `agy` in absolute directories on the daemon's `PATH`, then in the workload account's `~/.local/bin` on macOS and Linux or `%LOCALAPPDATA%\agy\bin` on Windows. `serve --antigravity /opt/example/bin/agy` takes precedence over `BOXEN_ANTIGRAVITY`. An explicit executable pins the choice, and `--antigravity auto` selects automatic discovery even if the environment sets a pin. Discovery is refreshed for availability checks and new ACP processes. [Google Antigravity CLI installation](https://antigravity.google/docs/cli-getting-started)

The workload account uses its existing Antigravity sign-in. Sign-in can be completed on that host with `agy acp --login`. The daemon does not collect browser sign-in codes or distribute provider credentials. Boxen requires ACP v1, HTTP MCP, and Antigravity 1.5.1 or newer. It launches `agy acp --process-per-session` for native HTTP MCP.

Use `start --backend antigravity` for anonymous work and `resident-create --backend antigravity` for resident activations. Model `-` selects the native default. Explicit model and thinking selections are checked against the options offered by that native session before prompting. The authenticated `runtime_info` operation reports Antigravity as configured when an executable is pinned or discovered. Sign-in and model availability are determined by the runtime when opening a session.

The desktop and browser model pickers use `antigravity_models` on the selected machine. The daemon opens a separate native ACP session under the workload account, reads its model options, and closes the session when supported. Discovery sends no prompt and creates no Boxen run. The successful catalog is cached for up to a minute for the same execution account and executable path. Native session settings remain authoritative when a run starts.

The same authenticated execution and watch operations provide ordered replay, exact-ID retries, cancellation, and conventional anonymous responses. The managed runtime receives retained Boxen attachments as file references. Antigravity advertises native thinking and message chunks and tool activity. Command output reported as `combinedOutput` is retained as aggregate output. Boxen supplies its run-scoped work or resident MCP tools through the existing bridge. Resident assistant narration is private, and outbound messages are explicit tool calls.

A completed native prompt can leave tools or a scheduled wake outstanding. In that case the Boxen run remains active. The journal records prompt completion separately from run completion. A late reply remains part of the originating run, including its tool and input activity. Antigravity reports the end of the reply through its usage update. A missing confirmation after Stop leaves an interrupted run with an uncertain outcome. Tool effects remain attributable to their original run.

An anonymous conversation can accept a fresh prompt on the same ACP connection
when Antigravity has returned `end_turn`, there are no outstanding questions or
steering requests, and the only open calls are native `run_command` calls with
an asynchronous wait budget. A new run owns the new prompt and the bridge calls
it makes. The old run still owns its command until a terminal update or an
explicit interrupted outcome. The wait budget establishes the kind of native
call Boxen can track independently. An accepted handoff requires its own receipt.
Work that can wake a model, including scheduled input and unresolved requests,
continues to gate follow-ups. The destination, execution account, working
directory, model and reasoning effort stay pinned while sharing the connection.

During an active native prompt, `steer` sends a new ACP prompt as live steering. Boxen advertises local ACP elicitation forms. A form arrives as an `input_requested` event with `request_id`, `kind: "form"`, and `requested_schema`. A `respond` result of `{"action":"accept","content":{"shape":"2"}}` submits the declared field and offered value. A result of `{"action":"decline"}` or `{"action":"cancel"}` closes that form. Whole-run cancellation sends the native stop request and cancels pending forms. Unsupported form shapes are declined with a retained event. Boxen accepts bounded flat forms with primitive fields and choice lists. Forms are for non-sensitive information. [ACP elicitation specification](https://agentclientprotocol.com/protocol/v1/elicitation)

Older fixed-choice requests carry `choices` and take `{"option_id":"offered-choice-id"}`. Both response formats use the exact JSON request ID from the journal. An invalid response produces a control error and leaves the request pending. Under Full access the adapter selects a one-time allow option for ordinary tool permission requests while keeping question choices distinct.

A new run resumes the existing native session when available. An explicit missing-session error or a recorded relocation opens a replacement session with bounded retained Boxen history. The journal preserves the recovery reason and previous binding. Native hidden state and omitted historical payloads remain unavailable. Earlier prompts and effectful tools are never dispatched as part of reconstruction.

Verification uses `scripts/antigravity-smoke.py` against the native and portable daemon. The Linux rotation includes this controlled ACP fixture. The optional live check `python3 scripts/codex-runtime-live.py target/release/boxen --antigravity /opt/example/bin/agy` exercises a real host-local account and can consume usage. It checks an actual shell effect and native resumption, then selective explicit mail and no-send completion through the daemon. Adding `--antigravity-form` exercises an actual Antigravity question and verifies that its selected value reaches the model’s completed response.

The CLI and authenticated API are the current Antigravity control surfaces. Open possibilities include native media transport, runtime-managed child-work evidence and handoff, and provider incidents that surface only inside native session storage.

[T3 Code Antigravity adapter](https://github.com/pingdotgg/t3code/blob/8cfd2b5211/apps/server/src/orchestration-v2/Adapters/AntigravityAdapterV2.ts) · [Google ACP documentation](https://antigravity.google/docs/cli/acp/)
