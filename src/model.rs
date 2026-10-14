//! Shared plain-data types.
//!
//! `Shortcut` lives here rather than in `launcher` because two modules need it:
//! the launcher produces them and the recency ranking orders them. While it sat
//! in `launcher`, `recents` had to import `launcher` — and `launcher` imports
//! `recents` to rank its own results — so the two were mutually dependent. This
//! module depends on nothing in the crate, which makes it a leaf both can use.
//!
//! Nothing here has behaviour: these are the shapes that cross module boundaries.

/// A launchable shortcut discovered in the Start Menu.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Shortcut {
    /// Display name (file stem of the shortcut).
    pub name: String,
    /// Absolute path to the `.lnk` (or executable).
    pub path: String,
}
