//! Exact text-width measurement, decoupled from any widget.
//!
//! Auto-fitting a column to "the largest piece of content or the label size"
//! needs the *rendered* pixel width of strings, which means real font metrics.
//! iced normally only exposes those inside a widget's `layout` (where a renderer
//! is in hand), but the text backend's font system is a process-global singleton
//! ([`font_system`]). [`Paragraph::with_text`] shapes against it directly, so we
//! can measure from anywhere — including `update`, where column widths are
//! recomputed — and get identical results to what the list cells will render.

use iced::advanced::graphics::text::Paragraph;
use iced::advanced::text::{self, Paragraph as _};
use iced::{Font, Pixels, Size};

/// Text size used by every details-list cell and column header. Auto-fit
/// measurement must use the same size as rendering, so this is the single source
/// of truth: the list cells set it explicitly and [`measure_width`] measures with
/// it. It matches iced's default text size, so it's not a visual change.
pub const LIST_TEXT_SIZE: f32 = 16.0;

/// The single-line rendered width, in logical pixels, of `content` at `size` in
/// the default font — the same shaping the list cells use, so the result lines
/// up exactly with what gets drawn (and with where [`crate::ellipsis`] truncates).
pub fn measure_width(content: &str, size: f32) -> f32 {
    let paragraph = Paragraph::with_text(text::Text {
        content,
        // Unbounded: we want the text's natural single-line width, not a fit.
        bounds: Size::new(f32::INFINITY, f32::INFINITY),
        size: Pixels(size),
        line_height: text::LineHeight::default(),
        font: Font::DEFAULT,
        align_x: text::Alignment::Default,
        align_y: iced::alignment::Vertical::Top,
        shaping: text::Shaping::default(),
        wrapping: text::Wrapping::None,
    });
    paragraph.min_width()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn longer_text_measures_wider() {
        // Exact pixel values depend on the bundled font, but width must grow
        // monotonically with content, and an empty string must be zero-width.
        assert_eq!(measure_width("", LIST_TEXT_SIZE), 0.0);
        let short = measure_width("file.txt", LIST_TEXT_SIZE);
        let long = measure_width("a-much-longer-file-name.txt", LIST_TEXT_SIZE);
        assert!(short > 0.0);
        assert!(long > short);
    }

    #[test]
    fn larger_size_measures_wider() {
        let small = measure_width("Report", 12.0);
        let big = measure_width("Report", 24.0);
        assert!(big > small);
    }
}
