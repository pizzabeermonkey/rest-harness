# Use the gpui-toolkit crates (crates.io) for the Pure Rust Native Desktop GUI

Status: accepted (supersedes the original "Use Iced" decision below)

## Decision

`rest-harness` remains a pure Rust native desktop GUI application, but the UI
is now built on the **gpui-toolkit crates from crates.io**:

- `gpui = "0.2.2"` — the windowing/rendering application framework,
- `gpui-ui-kit = "0.5.10"` — the component library the UI is composed from
  (`Button`, `Input`, `Badge`, `Heading`, `Text`, `Tabs`, `Select`,
  `Checkbox`, `VStack`, `HStack`, …),
- `gpui-design = "0.6.0"` — platform design tokens (spacing grid, corner
  radii, interaction rules) that drive the raw-`div` chrome.

## Why gpui-toolkit, and why not the alternatives

- **Supersedes iced** (`iced` 0.14, Elm-style `update`/`view`): iced served
  the project well, but the component ecosystem we wanted (a themed,
  batteries-included UI kit) lives in the gpui-toolkit ecosystem, and
  migrating keeps the same benefits the iced decision was based on: single
  native binary, no web runtime, pure-Rust stack, custom styling.
- **crates.io crates, not the git checkout**: the git dependency form of
  `gpui-miniapp` conflicts with `gpui-ios` over `core-foundation`, and
  `gpui-miniapp` is not published on crates.io anyway. The crates.io path
  compiles cleanly, so the app shell is written directly against
  `gpui::Application` plus `gpui-ui-kit` components.
- **Not bare gpui alone**: gpui is deliberately low-level (a div/element
  system, not a widget set). Building the request composer from
  `gpui-ui-kit`'s ready-made themed components is dramatically less code and
  keeps the UI consistent.

## Architecture unchanged

The UI-agnostic core (`rest_harness::app`: `AppState` + `Message` + `Effect`
reducer, tree projection, composer state machines) is untouched by this
decision — the `gpui` shell in `src/gui/` maps `Message` → reducer →
`Effect`s → async work on a dedicated tokio runtime, exactly as the iced
shell mapped effects to `iced::Task`s.

## One vendored patch

`gpui` (via `gpui_http_client` → `zed-async-tar`) pins `xattr 0.2.3`, which
no longer compiles against recent glibc (`libc` removed `ENOATTR`). The
repository vendors a patched copy under `vendor/xattr` (uses `ENODATA`) via a
`[patch.crates-io]` section. See `README.md` → Building from Source.

---

# Historical: Use Iced for Pure Rust Native Desktop GUI

Status: superseded by the decision above

We are building `rest-harness` as a pure Rust native desktop GUI application using `iced` rather than a web application, Electron/Tauri hybrid, or immediate-mode GUI (`egui`). `iced`'s Elm-inspired retained architecture (`State`, `Message`, `update`, `view`) fits a multi-tab, asynchronous HTTP client cleanly while shipping as a single native binary with custom styling and no bundled browser runtime.
