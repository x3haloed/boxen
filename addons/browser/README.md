# Boxen browser tools

This addon launches and debugs local Chromium browsers. Windows launch is
implemented for Chrome and Edge. It includes a standalone stdio MCP server.
Workload registration and assignment lifecycle wiring are separate slices.

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
