#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

#[path = "../../apps/agent/src/application.rs"]
mod application;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    application::main()
}
