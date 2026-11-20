#[path = "../apps/standalone-cli/src/application.rs"]
mod application;

fn main() -> std::process::ExitCode {
    application::main()
}
