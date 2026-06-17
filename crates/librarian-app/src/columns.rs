//! The details view's four columns — Name, Date modified, Type, Size — and how
//! each one's width is decided, plus the text encoding used to persist a
//! folder's layout.
//!
//! A column's width follows one of three [rules](ColRule): stretch to fill
//! (`Name` only, by default), auto-fit to the wider of its content or header
//! label, or a user-pinned pixel width from dragging its divider. Only folders
//! the user has actually customized are persisted; an all-default layout is
//! [`is_default`](ColumnLayout::is_default) and stored as nothing.

/// One of the four details columns. Used to address dividers, header cells, and
/// per-column rules without juggling indices.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Column {
    Name,
    Modified,
    Type,
    Size,
}

/// How a single column's width is determined.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ColRule {
    /// Absorb the pane's leftover width (never shrinking below a content-fit
    /// floor). Only `Name` defaults to this.
    Fill,
    /// Fit to the wider of the column's content or its header label.
    Auto,
    /// A width in logical pixels the user pinned by dragging the divider.
    Fixed(f32),
}

/// The sizing rules for all four columns of one folder.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ColumnLayout {
    pub name: ColRule,
    pub modified: ColRule,
    pub type_: ColRule,
    pub size: ColRule,
}

impl Default for ColumnLayout {
    /// The fresh-folder layout: Name fills, the rest fit their content.
    fn default() -> Self {
        Self {
            name: ColRule::Fill,
            modified: ColRule::Auto,
            type_: ColRule::Auto,
            size: ColRule::Auto,
        }
    }
}

impl ColumnLayout {
    /// Whether this is the untouched default (so it needn't be persisted).
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }

    pub fn rule(&self, col: Column) -> ColRule {
        match col {
            Column::Name => self.name,
            Column::Modified => self.modified,
            Column::Type => self.type_,
            Column::Size => self.size,
        }
    }

    pub fn set(&mut self, col: Column, rule: ColRule) {
        match col {
            Column::Name => self.name = rule,
            Column::Modified => self.modified = rule,
            Column::Type => self.type_ = rule,
            Column::Size => self.size = rule,
        }
    }
}

/// Encode a layout as four `;`-separated fields (`fill`, `auto`, or a number),
/// in column order. Pairs with [`decode_layout`].
pub fn encode_layout(layout: &ColumnLayout) -> String {
    let f = |rule: ColRule| match rule {
        ColRule::Fill => "fill".to_string(),
        ColRule::Auto => "auto".to_string(),
        // Round to whole pixels: sub-pixel precision isn't meaningful to persist.
        ColRule::Fixed(px) => format!("{}", px.round() as i64),
    };
    format!(
        "{};{};{};{}",
        f(layout.name),
        f(layout.modified),
        f(layout.type_),
        f(layout.size)
    )
}

/// Parse a layout encoded by [`encode_layout`]. Returns `None` on the wrong
/// field count or an unrecognized field, so a malformed line is skipped rather
/// than corrupting the rest of the store.
pub fn decode_layout(text: &str) -> Option<ColumnLayout> {
    let mut fields = text.split(';');
    let mut next = || fields.next().and_then(decode_rule);
    let name = next()?;
    let modified = next()?;
    let type_ = next()?;
    let size = next()?;
    // Reject trailing junk (more than four fields).
    if fields.next().is_some() {
        return None;
    }
    Some(ColumnLayout {
        name,
        modified,
        type_,
        size,
    })
}

fn decode_rule(field: &str) -> Option<ColRule> {
    match field {
        "fill" => Some(ColRule::Fill),
        "auto" => Some(ColRule::Auto),
        // Reject non-finite widths: a hand-edited/corrupt "nan"/"inf" entry
        // parses as a valid f32 but would propagate NaN through the layout math
        // (f32::clamp returns NaN unchanged) into iced's widget sizing.
        other => other
            .parse::<f32>()
            .ok()
            .filter(|w| w.is_finite())
            .map(ColRule::Fixed),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_a_customized_layout() {
        let layout = ColumnLayout {
            name: ColRule::Fixed(240.0),
            modified: ColRule::Auto,
            type_: ColRule::Fixed(120.0),
            size: ColRule::Fill,
        };
        assert_eq!(decode_layout(&encode_layout(&layout)), Some(layout));
    }

    #[test]
    fn default_layout_round_trips() {
        let layout = ColumnLayout::default();
        assert!(layout.is_default());
        assert_eq!(decode_layout(&encode_layout(&layout)), Some(layout));
    }

    #[test]
    fn rejects_malformed_encodings() {
        assert_eq!(decode_layout("fill;auto;auto"), None); // too few
        assert_eq!(decode_layout("fill;auto;auto;auto;auto"), None); // too many
        assert_eq!(decode_layout("fill;auto;bogus;auto"), None); // bad field
    }

    #[test]
    fn rejects_non_finite_fixed_widths() {
        // A corrupt/hand-edited config must not push a NaN/Inf width into layout.
        assert_eq!(decode_rule("nan"), None);
        assert_eq!(decode_rule("inf"), None);
        assert_eq!(decode_rule("-inf"), None);
        assert_eq!(decode_rule("240"), Some(ColRule::Fixed(240.0)));
    }

    #[test]
    fn set_and_rule_address_each_column() {
        let mut layout = ColumnLayout::default();
        layout.set(Column::Type, ColRule::Fixed(99.0));
        assert_eq!(layout.rule(Column::Type), ColRule::Fixed(99.0));
        assert_eq!(layout.rule(Column::Name), ColRule::Fill);
    }
}
