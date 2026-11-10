# Boxen browser tools

Boxen can attach this stdio MCP addon to a work-item activation or run it as a
standalone fixture. Chrome runs on macOS and Windows, and Edge runs on
Windows. Safari 27 and later use Apple's native Safari MCP server on macOS.

### Chrome and Edge

A profile lock excludes a second managed launch using the same directory. The
launcher checks that the loopback debugger socket in `DevToolsActivePort`
belongs to the process it launched before exposing CDP. The native owner asks
Chromium to flush the profile on shutdown. The profile persists for later
assignments.

Windows starts the browser suspended and owns its processes with a kill-on-close
Job Object. On macOS, a separate native owner starts Chrome in its own process
group and checks the listener with macOS's `lsof`. The owner holds the profile
lock until Chrome shuts down. Closing or killing the MCP process closes the
owner's control pipe, which triggers Chrome shutdown and bounded group cleanup.

Use the executable inside the installed application bundle on macOS, for example
`/Applications/Google Chrome.app/Contents/MacOS/Google Chrome`. Headed mode
supports initial sign-in from the logged-in desktop session. The same
dedicated profile path is reused for later launches. A pinned Chrome for
Testing bundle can also provide the executable. A Linux launcher remains
future work.

### Safari

On macOS, `executable` points to `/usr/bin/safaridriver`, and
`backend: "safari"` selects its native `--mcp` mode. Safari Settings >
Developer > Allow remote automation and external agents enables the local
driver. Safari can require the operator to authenticate this change. MCP discovery
lists the tools before a Safari window opens. Navigation is the first check
that the host can create an automation session.

Safari's own tools provide navigation, page content, interactions, JavaScript,
console and network inspection, and screenshots. Boxen retains Safari
screenshots as PNGs in the assigned evidence directory. Safari owns the
isolated automation window and its temporary session. The configured
`profile` directory is a Boxen reservation for this Safari slot. Safari is
headed and starts each automation session without persisted cookies or sign-in.
Chromium's raw CDP commands and breakpoint events are specific to Chrome and
Edge. See [WebKit's Safari MCP documentation](https://webkit.org/blog/18136/introducing-the-safari-mcp-server-for-web-developers/).

A connection accepts a loopback port and browser debugger path from the
browser owner. It keeps target session IDs valid across commands and retains
unsolicited events such as breakpoint hits. Event cursors expose loss from
the bounded buffer. A command sent without a matching reply has an uncertain
outcome and is never replayed automatically.

## Stdio MCP

`boxen-browser mcp ABSOLUTE_CONFIG_PATH` accepts a host-provided JSON file with
absolute `executable`, `profile`, and `evidence` paths. Chromium also
requires an absolute `agent_browser` executable and accepts `headless`
(default false). Safari uses `backend: "safari"` and omits `agent_browser`.
The browser starts on first tool use. EOF triggers cleanup.

Chromium exposes these tools over one browser endpoint:

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
`server` (the native Boxen browser addon executable), slots named `chrome`,
`edge` or `safari` with `executable` and `profile` fields, and optional
`headless`. All paths are absolute and host-local. The default is `chrome`.
`default: "edge"` or `default: "safari"` selects the corresponding slot.
`agent_browser` is required for Chrome and Edge. Safari uses a dedicated
reservation directory and requires headed mode.
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
delivery and retained PNG evidence, with an ordinary activation between the
browser activations. For Chromium it also checks listener cleanup. Pass
`--safari --executable /usr/bin/safaridriver` for the Safari path.

`tests/mcp-smoke.py` accepts `--boxen-browser`, `--agent-browser`,
`--executable`, and a fresh `--scratch` directory. It drives a local fixture
through the MCP protocol and checks navigation, Tab, literal input, a breakpoint
resolved from a TypeScript source map, paused-frame inspection, stepping,
resuming, a PNG artifact, and debugger endpoint closure. The Chromium test also checks
contention, cookie persistence and debugger cleanup after killing the MCP
process. Pass `--headed` to exercise the logged-in desktop session.
For Safari, replace `--agent-browser` with `--safari` and pass
`--executable /usr/bin/safaridriver`. Its smoke checks navigation, page
inspection, input, console and network inspection, and PNG evidence.
No application sign-in or cloud service is required.

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
