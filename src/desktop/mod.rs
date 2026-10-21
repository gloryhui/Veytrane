//! Desktop presentation: Agent IPC is the default; embedded runtime is explicit.
mod actions;
pub mod agent_shell;
mod legacy;
mod platform;
mod ui;

// Preserve the explicit embedded-desktop library entry point.
pub use legacy::run;
