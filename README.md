<p align="center">
  <img src="assets/logo.svg" alt="REST Harness Logo" width="140" />
</p>

# REST Harness

A pure-Rust, git-friendly desktop REST API client for Linux and Windows.

[![CI](https://github.com/pizzabeermonkey/rest-harness/actions/workflows/ci.yml/badge.svg)](https://github.com/pizzabeermonkey/rest-harness/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)

## Overview & Key Features

- **Pure-Rust native desktop GUI** built on the [`gpui-toolkit`](https://crates.io/crates/gpui) crates (`gpui` + `gpui-ui-kit` + `gpui-design`) — themed components, no bundled web runtime.
- **Git-friendly YAML workspace** — `Workspace → Collection → nested Folder → Request`, one human-readable YAML file per artifact. Diff, review, and version your API calls like code.
- **Collection-scoped Environments** with `{{variable}}` interpolation and fail-fast validation: referencing an unknown variable is an error, not a silent empty string.
- **Isolated secrets** — each Collection keeps a gitignored `.secrets.yaml`; secret values are referenced with `{{variable}}` placeholders in request files and are automatically redacted to `***` in the local `.rest-harness-history.yaml` execution log.
- **Two-way live URL ↔ Query Params sync** — edit either side and the other updates instantly, with percent-encoding and disabled-row preservation.
- **Postman-style Request Composer & Response Inspector** — Params/Headers/Body tabs, an environment selector, a status-code badge, response headers and a Pretty/Raw body view.

## The GUI

The desktop app is a `gpui` window composed from `gpui-ui-kit` components:

- **Top bar** — the open workspace root plus `+ Draft` and `Reload`
  (`Ctrl+N`, `Ctrl+R`).
- **Sidebar** — a flat, filterable list of every Request in the workspace
  (Collections and nested Folders walked recursively), plus the recent
  execution History. Clicking a Request opens it; clicking a History entry
  opens a prefilled draft.
- **Composer** — one tab per open Request (`Ctrl+S` saves, `Ctrl+Enter`
  sends, `Esc`…). The editor has a method picker, the URL field and the
  classic **Params / Headers / Body** sub-tabs; Params and Headers are
  editable key/value rows whose enabled state is toggled per row, and the
  URL and Params table stay in sync two-way. An environment selector picks
  "No environment", a Collection (shared secrets only) or one of its
  Environments; `{{variables}}` interpolate and fail fast when undefined.
- **Response inspector** — a status badge, elapsed time and size, a
  `Headers / Body` tab pair and a Pretty/Raw body view with copy support.
- **Status strip** — the last error/info line from the reducer.

Quitting with dirty tabs is guarded by a Save/Discard/Cancel dialog, and
drafts are saved through the save-draft dialog (which never overwrites an
existing Request).

## Installation

### Prebuilt binaries (recommended)

Download the archive for your platform from the [GitHub Actions build artifacts](https://github.com/pizzabeermonkey/rest-harness/actions/workflows/ci.yml) (every CI run uploads them) or from Releases:

- **Linux x64:** `rest-harness-linux-x86_64.tar.gz`
- **Windows x64:** `rest-harness-windows-x86_64.zip`

```bash
# Linux
tar -xzf rest-harness-linux-x86_64.tar.gz -C rest-harness-dist
./rest-harness-dist/rest-harness
```

```powershell
# Windows
Expand-Archive rest-harness-windows-x86_64.zip -DestinationPath rest-harness-dist
.\rest-harness-dist\rest-harness.exe
```

Each archive contains the binary, `README.md`, `LICENSE`, and the sample `collections/` directory.

### From a local checkout

```bash
cargo install --locked --path .
```

## Building from Source

**Prerequisites:**

- [Rust](https://rustup.rs) stable toolchain
- **Linux:** GPUI's dev packages —
  `pkg-config libfontconfig1-dev libfreetype6-dev libwayland-dev libx11-xcb-dev libxkbcommon-dev libxkbcommon-x11-dev libxcb1-dev libexpat1-dev`
  (e.g. `sudo apt-get install -y pkg-config libfontconfig1-dev libfreetype6-dev libwayland-dev libx11-xcb-dev libxkbcommon-dev libxkbcommon-x11-dev libxcb1-dev libexpat1-dev`)
- **Windows:** MSVC Build Tools (installed with the default Visual Studio `x86_64-pc-windows-msvc` toolchain)

> **Vendored patch:** the repository carries a patched copy of the `xattr`
> crate under `vendor/xattr`, wired up through a `[patch.crates-io]` section
> in `Cargo.toml`. It exists because `gpui` (via `gpui_http_client` →
> `zed-async-tar`) pins `xattr 0.2.3`, which no longer compiles on recent
> glibc systems (`libc` removed the `ENOATTR` constant; the vendored copy
> uses `ENODATA`).

**Build & run:**

```bash
# Linux x64
cargo build --release --locked --target x86_64-unknown-linux-gnu
target/x86_64-unknown-linux-gnu/release/rest-harness
```

```powershell
# Windows x64
cargo build --release --locked --target x86_64-pc-windows-msvc
target\x86_64-pc-windows-msvc\release\rest-harness.exe
```

**Run the test suite:**

```bash
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --all -- --check
```

## Workspace & File Format Guide

A workspace root is just a directory. REST Harness creates the structure on first
use and keeps everything as plain YAML files:

```text
<workspace root>/
├── .gitignore                    # ignores secrets + local history
├── .rest-harness-history.yaml    # gitignored execution history (auto-redacted)
└── collections/                  # one directory per Collection
    └── example-api/
        ├── .secrets.yaml         # gitignored Collection-local secrets
        ├── environments/         # one flat YAML file per Environment
        │   ├── dev.yaml
        │   └── prod.yaml
        └── <Request name>.yaml   # one YAML file per Request
```

### Example Request (`collections/example-api/get-headers.yaml`)

```yaml
name: Get headers
method: get
url: "{{base_url}}/headers"
params:
  - key: verbose
    value: "1"
    description: Demo query parameter
    enabled: true
  - key: trace
    value: "on"
    description: Disabled row — kept in the file, excluded from the URL
    enabled: false
headers:
  - key: Accept
    value: application/json
    description: ""
    enabled: true
```

Bodies and auth use the same shapes — e.g. a POST with a raw JSON body and a
bearer token referenced (never stored) via a variable:

```yaml
auth: !bearer
  token: "{{api_token}}"
body: !raw
  content_type: json
  content: |
    {
      "message": "Hello from REST Harness"
    }
```

### Example Environment (`environments/dev.yaml`)

Environments are **flat** YAML mappings — the file name is the Environment's
name. The sample collection also ships a `prod.yaml` with a different
`base_url`, so the same requests can be switched between environments.

```yaml
base_url: https://httpbin.org
api_token: local-dev-token
```

### Example `.secrets.yaml`

Secret values live only in this gitignored, Collection-local file: flat
top-level keys are shared across all Environments, and the `environments:`
mapping holds per-Environment overrides. Request files only ever reference them
by `{{variable}}` name.

```yaml
api_token: replace-with-your-local-secret
secrets:
  another_key: placeholder-value
environments:
  prod:
    api_token: replace-with-prod-secret
```

---

Licensed under the [MIT License](LICENSE).
