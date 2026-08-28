// macOS integration uses the Accessibility API through rdev/enigo. The
// adapter is intentionally isolated so the portable crates never depend on
// Apple frameworks.

#[cfg(target_os = "macos")]
mod macos;

#[cfg(target_os = "macos")]
pub use macos::*;
