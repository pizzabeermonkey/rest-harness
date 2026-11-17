//! REST Harness desktop entry point — a thin bootstrap. All program wiring
//! (boot/update/view/subscription) lives in the `gui` module (`src/gui/`);
//! the UI-agnostic reducer lives in the lib crate (`rest_harness::app`).

mod gui;

fn main() -> iced::Result {
    if std::env::args().nth(1).as_deref() == Some("--version") {
        println!("rest-harness {}", rest_harness::VERSION);
        return Ok(());
    }

    iced::application(gui::boot, gui::update, gui::view)
        .title("REST Harness")
        .theme(|_: &gui::App| iced::Theme::Dark)
        .window_size(iced::Size::new(1200.0, 800.0))
        .subscription(gui::subscription)
        .run()
}
