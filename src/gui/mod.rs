//! This module contains code relating to GUI.

// public members
pub mod app;
pub mod backlog;
pub mod notify;
#[cfg(target_os = "linux")]
pub mod tray;

// private members with re-export
