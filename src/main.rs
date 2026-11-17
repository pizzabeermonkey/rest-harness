//! REST Harness desktop entry point — a thin bootstrap. All program wiring
//! (boot/update/view/subscription) lives in the `gui` module (`src/gui/`);
//! the UI-agnostic reducer lives in the lib crate (`rest_harness::app`).

use std::path::PathBuf;

mod gui;

fn main() -> iced::Result {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("--version") {
        println!("rest-harness {}", rest_harness::VERSION);
        return Ok(());
    }

    // Headless screenshot mode: render the real UI offscreen and exit,
    // without opening a window (works without X11/Wayland).
    if args.get(1).map(String::as_str) == Some("--screenshot") {
        if let Err(error) = run_screenshot(args.get(2).map(String::as_str)) {
            eprintln!("rest-harness: {error}");
            std::process::exit(1);
        }
        return Ok(());
    }

    iced::application(gui::boot, gui::update, gui::view)
        .title("REST Harness")
        .theme(|_: &gui::App| iced::Theme::Dark)
        .default_font(iced::Font::with_name("Fira Sans"))
        .window_size(iced::Size::new(1200.0, 800.0))
        .subscription(gui::subscription)
        .run()
}

/// Run the headless screenshot mode inside a small tokio runtime.
fn run_screenshot(output: Option<&str>) -> Result<(), String> {
    let output = output
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("assets/screenshot.png"));
    let runtime = tokio::runtime::Runtime::new()
        .map_err(|error| format!("could not start the tokio runtime: {error}"))?;
    runtime.block_on(gui::screenshot::run(&output))?;
    println!("Screenshot written to {}", output.display());
    Ok(())
}
