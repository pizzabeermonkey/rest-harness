# Use Iced for Pure Rust Native Desktop GUI

Status: accepted

We are building `rest-harness` as a pure Rust native desktop GUI application using `iced` rather than a web application, Electron/Tauri hybrid, or immediate-mode GUI (`egui`). `iced`'s Elm-inspired retained architecture (`State`, `Message`, `update`, `view`) fits a multi-tab, asynchronous HTTP client cleanly while shipping as a single native binary with custom styling and no bundled browser runtime.
