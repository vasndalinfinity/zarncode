//! Color constants for consistent UI styling across the application.

use ratatui::style::Color;

/// Primary accent color - used for active elements and highlights
pub const PRIMARY: Color = Color::Rgb(215, 165, 125);

#[allow(dead_code)]
/// Secondary accent color - used for prompts and success states
pub const SECONDARY: Color = Color::Rgb(156, 191, 151);

/// Highlight color - used for important elements and keyboard shortcuts
pub const HIGHLIGHT: Color = Color::Rgb(225, 189, 123);

#[allow(dead_code)]
/// Error color - used for error messages and failed states
pub const ERROR: Color = Color::Rgb(225, 133, 133);

/// Muted text color - used for secondary information
pub const MUTED: Color = Color::Rgb(167, 164, 174);

/// Subtle text color - used for hints and metadata
pub const SUBTLE: Color = Color::Rgb(128, 126, 139);

/// Default text color
pub const TEXT: Color = Color::Rgb(229, 226, 220);

/// Blue color - used for tool names in tool call indicators
pub const BLUE: Color = Color::Rgb(145, 178, 211);

/// Green color - used for success indicators (bullet points, etc.)
pub const GREEN: Color = Color::Rgb(156, 191, 151);

/// Warning color - used for compaction warnings and alerts
pub const WARNING: Color = Color::Rgb(225, 189, 123);

/// Critical/Error color - used for critical compaction alerts
pub const CRITICAL: Color = Color::Rgb(225, 133, 133);

/// Terminal canvas.
pub const BACKGROUND: Color = Color::Rgb(23, 23, 28);
/// Raised input and user-message surface.
pub const SURFACE: Color = Color::Rgb(35, 35, 42);
