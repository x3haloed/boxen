# Rust desktop framework choice

Research date: 2026-11-01. This choice serves a small, playable docking workspace;
it does not prescribe Boxen's eventual visual design.

| Candidate | Relevant current evidence | Fit for this slice |
| --- | --- | --- |
| [egui / eframe](https://github.com/emilk/egui) | Native macOS/Windows/Linux, custom drawing/widgets, wgpu renderer, AccessKit. [Recent releases](https://github.com/emilk/egui/releases) include inspection/IME work in 0.35 and 0.37's colored emoji support. | Flexible presentation, an established integration surface, and small application-owned interaction state. Immediate-mode rendering requires attention to idle repainting and large histories. |
| [egui_dock](https://github.com/anhosh/egui_dock) | Tabs, nested splits, resizing, cross-group moves, floating egui windows, serializable layouts and per-tab customization. | Direct fit for a common docking tree containing both the thread list and conversation panes. Preview semantics remain Boxen-owned. Floating egui surfaces are not OS windows. |
| [GPUI Kit](https://github.com/longbridge/gpui-kit) | Now combines GPUI, unstyled behavior infrastructure and components; documents GPU rendering, virtual lists, docking with edge docks/nested splits, accessibility and headless UI testing. | Strong alternative, especially for extensive rich-text/virtualized UI. A broader application/component system than this first slice needs. Its advertised FPS is not a Boxen benchmark. |
| [Iced](https://github.com/iced-rs/iced/releases) | 0.14 adds reactive rendering, headless/end-to-end testing, IME and debugging facilities. PaneGrid provides split/resize/drag behavior. | Credible general UI foundation. Complete editor-style tabbed docking would require more application-level composition than the chosen docking library. |

Selected: **eframe 0.35.0 + egui_dock 0.20.1**, with wgpu and AccessKit enabled.
These are a matching published pair compatible with the installed Rust 1.94
compiler. The current newest releases are eframe 0.37.0 and egui_dock 0.21.1;
they require Rust 1.95, and the latter targets egui 0.36 rather than 0.37.
See the [current docking manifest](https://github.com/anhosh/egui_dock/blob/main/Cargo.toml).
A matching dependency graph is preferable to mixing incompatible egui versions.
This is a version pin for the prototype, not a decision against future upgrades.

The desktop is an independent Cargo package, keeping window-system/GPU dependencies
out of the single-file portable daemon. It attaches through the existing API;
there is no second workload, identity, pairing or continuity authority. Worker
publication requests repaint only when data arrives; there is no unconditional
continuous render loop. Large-history virtualization and separate native floating
windows remain future work; framework claims alone do not establish their fitness
or performance in Boxen.
