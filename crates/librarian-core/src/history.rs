//! Back/forward navigation history for a single browsing context (one tab).

use crate::model::Location;

/// A classic browser-style history: a current location with back and forward
/// stacks. Navigating to a new location clears the forward stack.
#[derive(Debug, Clone)]
pub struct History {
    back: Vec<Location>,
    forward: Vec<Location>,
    current: Location,
}

impl History {
    pub fn new(start: Location) -> Self {
        Self {
            back: Vec::new(),
            forward: Vec::new(),
            current: start,
        }
    }

    pub fn current(&self) -> &Location {
        &self.current
    }

    /// Navigate to a new location. No-op if it equals the current location
    /// (avoids polluting history with refreshes of the same place).
    pub fn navigate(&mut self, to: Location) {
        if to == self.current {
            return;
        }
        let previous = std::mem::replace(&mut self.current, to);
        self.back.push(previous);
        self.forward.clear();
    }

    pub fn can_go_back(&self) -> bool {
        !self.back.is_empty()
    }

    pub fn can_go_forward(&self) -> bool {
        !self.forward.is_empty()
    }

    /// Move one step back, returning the new current location.
    pub fn go_back(&mut self) -> Option<&Location> {
        let target = self.back.pop()?;
        let previous = std::mem::replace(&mut self.current, target);
        self.forward.push(previous);
        Some(&self.current)
    }

    /// Move one step forward, returning the new current location.
    pub fn go_forward(&mut self) -> Option<&Location> {
        let target = self.forward.pop()?;
        let previous = std::mem::replace(&mut self.current, target);
        self.back.push(previous);
        Some(&self.current)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn path(s: &str) -> Location {
        Location::Path(PathBuf::from(s))
    }

    #[test]
    fn navigate_pushes_back_and_clears_forward() {
        let mut h = History::new(Location::ThisPc);
        h.navigate(path(r"C:\"));
        h.navigate(path(r"C:\Users"));
        assert!(h.can_go_back());
        assert!(!h.can_go_forward());

        h.go_back();
        assert_eq!(h.current(), &path(r"C:\"));
        assert!(h.can_go_forward());

        // Navigating from a back position drops the forward history.
        h.navigate(path(r"C:\Windows"));
        assert!(!h.can_go_forward());
        assert_eq!(h.current(), &path(r"C:\Windows"));
    }

    #[test]
    fn back_and_forward_are_symmetric() {
        let mut h = History::new(Location::ThisPc);
        h.navigate(path(r"C:\"));
        h.go_back();
        assert_eq!(h.current(), &Location::ThisPc);
        h.go_forward();
        assert_eq!(h.current(), &path(r"C:\"));
    }

    #[test]
    fn navigate_to_same_location_is_noop() {
        let mut h = History::new(path(r"C:\"));
        h.navigate(path(r"C:\"));
        assert!(!h.can_go_back());
    }
}
