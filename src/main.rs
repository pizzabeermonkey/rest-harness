mod gui;

fn main() {
    if std::env::args().nth(1).as_deref() == Some("--version") {
        println!("rest-harness {}", rest_harness::VERSION);
        return;
    }
    gui::run();
}
