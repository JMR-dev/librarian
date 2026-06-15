//! A single-line text widget that truncates with an ellipsis ("…") when it
//! doesn't fit its width, instead of wrapping to a second line.
//!
//! iced 0.14 has no built-in ellipsis: [`Wrapping::None`](iced::advanced::text::Wrapping)
//! keeps text on one line but lets it overflow its bounds, and there's no
//! truncation strategy. This widget fills that gap. The only place a renderer
//! (and thus real font metrics) is available is a widget's `layout`, so that's
//! where it measures: it lays the full string out on one line, and if that
//! exceeds the available width it binary-searches the longest character prefix
//! that fits with an ellipsis appended. Measuring against the actual font keeps
//! truncation exact regardless of glyph widths.
//!
//! Everything else is delegated to iced's own text `layout`/`draw`/`State`, so
//! the only new logic is the measure-and-truncate step.

use iced::advanced::text::paragraph::Paragraph;
use iced::advanced::widget::text as text_widget;
use iced::advanced::widget::{Tree, tree};
use iced::advanced::{Layout, Widget, layout, mouse, renderer, text};
use iced::{Element, Length, Pixels, Rectangle, Size};

/// Create an ellipsizing single-line text cell with the given content.
pub fn ellipsized(content: impl Into<String>) -> Ellipsized {
    Ellipsized {
        content: content.into(),
        width: Length::Shrink,
        size: None,
        align_x: text::Alignment::Default,
    }
}

/// A single-line, ellipsis-truncating text widget. See the [module
/// docs](self).
pub struct Ellipsized {
    content: String,
    width: Length,
    size: Option<Pixels>,
    align_x: text::Alignment,
}

impl Ellipsized {
    /// Sets the width the text is fit into (truncation happens at this width).
    pub fn width(mut self, width: impl Into<Length>) -> Self {
        self.width = width.into();
        self
    }

    /// Sets the text size; defaults to the renderer's default.
    pub fn size(mut self, size: impl Into<Pixels>) -> Self {
        self.size = Some(size.into());
        self
    }

    /// Sets the horizontal alignment within the cell — e.g. right-align a
    /// numeric column. Truncation (prefix + trailing "…") is unchanged; only
    /// where the resulting line sits within the cell differs.
    pub fn align_x(mut self, alignment: impl Into<text::Alignment>) -> Self {
        self.align_x = alignment.into();
        self
    }

    /// The shared text [`Format`](text_widget::Format) for both measuring and
    /// laying out — top aligned, never wrapping.
    fn format<Font>(&self) -> text_widget::Format<Font> {
        text_widget::Format {
            width: self.width,
            height: Length::Shrink,
            size: self.size,
            font: None,
            line_height: text::LineHeight::default(),
            align_x: self.align_x,
            align_y: iced::alignment::Vertical::Top,
            shaping: text::Shaping::default(),
            wrapping: text::Wrapping::None,
        }
    }
}

/// The widget's per-instance state is just iced's text paragraph cache.
type State<R> = text_widget::State<<R as text::Renderer>::Paragraph>;

impl<Message, Theme, Renderer> Widget<Message, Theme, Renderer> for Ellipsized
where
    Renderer: text::Renderer,
{
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<State<Renderer>>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(State::<Renderer>::default())
    }

    fn size(&self) -> Size<Length> {
        Size {
            width: self.width,
            height: Length::Shrink,
        }
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        let state = tree.state.downcast_mut::<State<Renderer>>();
        let format = self.format::<Renderer::Font>();

        // Lay out the full string first (single line). The resulting node's
        // width is the cell's *real* width — the fixed column width, or the
        // fill allocation — which is what we must fit into. (The incoming
        // `limits.max().width` can be larger than a fixed column, so it can't
        // be used for the fit test.) If the text fits, we're done — the common
        // case costs exactly what a plain text widget would.
        let node = text_widget::layout(state, renderer, limits, &self.content, format);
        let avail = node.size().width;
        if state.min_bounds().width <= avail {
            return node;
        }
        // It overflows: truncate to the longest prefix that fits with "…".
        let display = ellipsize::<Renderer>(&self.content, avail, renderer, format);
        text_widget::layout(state, renderer, limits, &display, format)
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut Renderer,
        _theme: &Theme,
        defaults: &renderer::Style,
        layout: Layout<'_>,
        _cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        let state = tree.state.downcast_ref::<State<Renderer>>();
        text_widget::draw(
            renderer,
            defaults,
            layout.bounds(),
            state.raw(),
            // No explicit color: inherit the surrounding text color (so selected
            // rows still recolor their text via the row container's style).
            text_widget::Style { color: None },
            viewport,
        );
    }
}

/// The longest character prefix of `content` that fits within `avail` pixels
/// once an ellipsis is appended, measured against the actual font. Returns at
/// least `"…"`.
fn ellipsize<Renderer>(
    content: &str,
    avail: f32,
    renderer: &Renderer,
    format: text_widget::Format<Renderer::Font>,
) -> String
where
    Renderer: text::Renderer,
{
    let size = format.size.unwrap_or_else(|| renderer.default_size());
    let font = format.font.unwrap_or_else(|| renderer.default_font());

    // Single-line pixel width of a candidate string, via a throwaway paragraph.
    let measure = |candidate: &str| -> f32 {
        <Renderer::Paragraph as Paragraph>::with_text(text::Text {
            content: candidate,
            bounds: Size::new(f32::INFINITY, f32::INFINITY),
            size,
            line_height: format.line_height,
            font,
            align_x: format.align_x,
            align_y: format.align_y,
            shaping: format.shaping,
            wrapping: text::Wrapping::None,
        })
        .min_width()
    };

    let chars: Vec<(usize, char)> = content.char_indices().collect();
    let n = chars.len();
    let byte_of = |m: usize| if m >= n { content.len() } else { chars[m].0 };

    // Width grows monotonically with the prefix length, so binary-search the
    // largest m in [0, n] whose prefix + "…" still fits.
    let mut lo = 0usize;
    let mut hi = n;
    let mut best = 0usize;
    while lo <= hi {
        let mid = (lo + hi) / 2;
        let candidate = format!("{}…", &content[..byte_of(mid)]);
        if measure(&candidate) <= avail {
            best = mid;
            if mid == n {
                break;
            }
            lo = mid + 1;
        } else if mid == 0 {
            break;
        } else {
            hi = mid - 1;
        }
    }
    format!("{}…", &content[..byte_of(best)])
}

impl<'a, Message, Theme, Renderer> From<Ellipsized> for Element<'a, Message, Theme, Renderer>
where
    Renderer: text::Renderer + 'a,
    Theme: 'a,
    Message: 'a,
{
    fn from(widget: Ellipsized) -> Self {
        Element::new(widget)
    }
}
