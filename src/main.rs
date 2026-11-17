//! Minimal placeholder binary for REST Harness v1.
//!
//! The `iced` GUI arrives in a later task; for now the binary prints the
//! crate version and usage so the package builds as an application.

fn main() {
    if std::env::args().nth(1).as_deref() == Some("--version") {
        println!("rest-harness {}", rest_harness::VERSION);
        return;
    }
    println!(
        "rest-harness {} (GUI coming in a later task)",
        rest_harness::VERSION
    );
    println!();
    println!("usage: rest-harness [--version]");
    println!();
    println!("Library crate `rest_harness` provides the domain model, YAML");
    println!("workspace storage, secrets and history persistence.");
}
