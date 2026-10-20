//! Reusable product capabilities; UI and platform adapters have separate roles.
pub mod palette;
#[cfg(windows)]
pub mod task_access;
#[cfg(windows)]
pub mod task_launch;
pub mod task_service;
#[cfg(windows)]
pub mod task_ui;
#[cfg(windows)]
#[allow(dead_code)] // Shared legacy Win32 helpers; each native host uses a subset.
mod win_util;
