//! The app's semantic colors in one place — the surface to change for future
//! theming or user preferences.
//!
//! Anything that should track "the accent" (the column dividers today; more
//! later) reads it from here rather than reaching into the iced palette directly,
//! so re-theming is a single edit now and an easy hook for a settings-driven
//! palette later.

use iced::{Color, Theme};

/// The app's accent color — its primary highlight, shared with buttons, the
/// selection, and focus rings. Currently the active iced theme's primary; change
/// this (or, later, source it from user preferences) to retheme the accent.
pub fn accent(theme: &Theme) -> Color {
    theme.extended_palette().primary.strong.color
}

/// The color of the details view's column/header dividers — both the vertical
/// column rules and the horizontal rule beneath the header. Tied to [`accent`]
/// so the table's lines match the rest of the app.
pub fn divider(theme: &Theme) -> Color {
    accent(theme)
}
