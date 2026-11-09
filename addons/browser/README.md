# Boxen browser tools

This addon launches and debugs local Chromium browsers. Windows launch is
implemented for Chrome and Edge. Boxen can run the stdio MCP server for an
assignment activation. An operator can also run it as a standalone fixture.

`Browser::launch` takes an absolute executable and a dedicated profile directory.
The caller chooses headed or headless mode. A profile lock excludes another
managed launch using the same directory. Windows starts the browser suspended,
places it in a kill-on-close Job Object, then resumes it. The launcher checks
that the resulting loopback debugger socket belongs to that browser process
before publishing its endpoint. `Browser::close` allows Chromium to flush the
profile and waits for its process tree. The Job Object also covers unexpected
owner exit. The profile directory remains for later assignments.

The process adapter lives in `src/browser/windows.rs`. Future host adapters
implement process-tree ownership, debugger listener verification, and bounded
shutdown for their host. The CDP command and event interfaces are host-neutral.

### macOS and Linux handoff

The launcher contract is in `src/browser.rs`. Each host needs an adapter that
starts the selected browser, detects premature exit, proves that the loopback
listener in the profile's `DevToolsActivePort` belongs to the launched process,
and waits for or terminates its owned process tree. Keep the profile lock until
shutdown finishes. Launching into an already-running browser or attaching to an
unrelated listener would violate the assignment's ownership contract.

The host work includes selecting the correct Chrome for Testing package for its
architecture, finding the executable inside a macOS application bundle or
Linux package, and running headed mode in the operator's desktop session for
initial sign-in. The same profile path is reused for later runs on that host.
The assignment registry, MCP transport, agent-browser adapter, and CDP
connection are shared. Browser binaries and profiles live outside the daemon's
portable executable.

The local MCP and real Codex smoke scripts below cover the shared behavior.
Host acceptance also needs to exercise profile reuse after a graceful restart,
contention on the same profile, and process-tree cleanup when the browser owner
exits abruptly. The current managed lifecycle test is Windows-gated. The
standalone MCP fixture is portable once the host launcher is in place.

A connection accepts a loopback port and browser debugger path from the
browser owner. It keeps target session IDs valid across commands and retains
unsolicited events such as breakpoint hits. Event cursors expose loss from
the bounded buffer. A command sent without a matching reply has an uncertain
outcome and is never replayed automatically.

## Stdio MCP

`boxen-browser mcp ABSOLUTE_CONFIG_PATH` accepts a host-provided JSON file with
`executable`, `agent_browser`, `profile`, `evidence` and optional `headless`
(default false). The four paths are absolute. Each server process owns one
browser session and uses the same endpoint for automation and native debugger
commands. The browser starts on first tool use. EOF triggers cleanup.

- `browser_action` provides navigation, accessibility snapshots, input,
  Back/Forward, reload, and PNG capture. It calls a local agent-browser binary
  through its typed MCP interface, verified with agent-browser 0.41.2.
- `browser_cdp` accepts a native method, params and optional flattened target
  session ID. It returns the original CDP result or error. A TypeScript
  breakpoint uses the script's source map to identify the generated location.
- `browser_events` polls the same connection for events after a cursor.
  `dropped_through` reports any evicted history.

Screenshots and oversized responses are saved under `evidence`. Raw CDP results
can contain authenticated browser state. Config and evidence stay on the local
host under the assignment owner's control.

### Work-item assignment

An optional `project.managed_browser` object in the work-item workflow
configuration enables this path for newly allocated assignments. It accepts
`server` (the installed Boxen browser addon executable), `agent_browser`,
`chrome` and optional `edge` slots with `executable` and `profile` fields,
and optional `headless`. All paths are absolute and host-local. The default is
`chrome`, intended to point to a pinned Chrome for Testing installation.
`default: "edge"` selects the configured Edge slot. Chrome and Edge profiles
are dedicated to their respective installations and persist across assignments.
An optional `items` list limits the managed browser to specified work-item IDs
during a pilot. An omitted or empty list applies to all new workflow allocations.

Work-item dispatch retains the chosen browser, profile and evidence paths in
the assignment. While holding its profile claim, it publishes the immutable
grant in the daemon's `browser-assignments` registry before submitting the
assignment mail. Boxen attaches the server when that exact message is in the
activation batch and its authenticated sender and recipient match the grant.
A continuation gets a new grant with its new assignment message. Ordinary
resident mail does not attach the tool. A conflicting grant fails the
activation before the model turn starts.

The old browser launch path is selected when `project.managed_browser` is
absent. Existing assignments retain their frozen settings when the operator
changes this configuration. Capacity release waits for managed browser
profile shutdown in addition to the existing idle and presentation checks.

The opt-in `scripts/codex-browser-live.py` starts an isolated Boxen daemon
and uses the installed Codex sign-in. It checks first-use and resumed tool
delivery, retained PNG evidence and listener cleanup, with an ordinary
activation between the browser activations.

`tests/mcp-smoke.py` accepts `--boxen-browser`, `--agent-browser`,
`--executable`, and a fresh `--scratch` directory. It drives a local fixture
through the MCP protocol and checks navigation, Tab, literal input, a breakpoint
resolved from a TypeScript source map, paused-frame inspection, stepping,
resuming, a PNG artifact, and debugger endpoint closure. No application or
cloud service is required.

## Transport and lifecycle tests

`cargo test --manifest-path addons/browser/Cargo.toml --locked` runs the
transport fixtures. The ignored test `real_browser_breakpoint_and_step`
accepts `BOXEN_BROWSER_ACTIVE_PORT`, the absolute path to the
`DevToolsActivePort` file of a dedicated Chrome or Edge test process. It opens
a blank test tab, hits a conditional JavaScript breakpoint, inspects a local
variable, steps, resumes, and closes the tab. Start the test browser with a
disposable user data directory and `--remote-debugging-port=0`. The test
assumes a local test process under the operator's control.

On Windows, the ignored `managed_browser_reserves_reuses_and_releases_profile`
test takes `BOXEN_BROWSER_EXECUTABLE` and `BOXEN_BROWSER_TEST_PROFILE`. The
profile path must be absolute and absent before the test. The test checks a
conditional breakpoint, conflicting launches, persistence of a synthetic
cookie after graceful shutdown, and debugger endpoint closure. It retains the
disposable test directory. These checks establish browser lifecycle behavior.
An authenticated application pilot exercises sign-in separately.
