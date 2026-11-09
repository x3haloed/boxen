# Boxen browser tools

This addon contains the shared transport for controlling a local Chromium
browser over the Chrome DevTools Protocol. The current slice is a Rust
library. Its host integration, browser launch and profile ownership, and
assignment-scoped MCP server are still in progress.

A connection accepts a loopback port and browser debugger path from the
browser owner. It keeps target session IDs valid across commands and retains
unsolicited events such as breakpoint hits. Event cursors expose loss from
the bounded buffer. A command sent without a matching reply has an uncertain
outcome and is never replayed automatically.

`cargo test --manifest-path addons/browser/Cargo.toml --locked` runs the
transport fixtures. The ignored test `real_browser_breakpoint_and_step`
accepts `BOXEN_BROWSER_ACTIVE_PORT`, the absolute path to the
`DevToolsActivePort` file of a dedicated Chrome or Edge test process. It opens
a blank test tab, hits a conditional JavaScript breakpoint, inspects a local
variable, steps, resumes, and closes the tab. Start the test browser with a
disposable user data directory and `--remote-debugging-port=0`. The test
assumes a local test process under the operator's control.
