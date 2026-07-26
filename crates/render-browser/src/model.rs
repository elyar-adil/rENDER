//! Browser tab state independent of rendering and networking.

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TabId(u64);

impl TabId {
    #[must_use]
    pub const fn as_u64(self) -> u64 {
        self.0
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tab {
    pub id: TabId,
    pub title: String,
    pub address: String,
    pub loading: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TabIntent {
    New,
    Close(TabId),
    Activate(TabId),
    Move { tab: TabId, index: usize },
}

#[derive(Clone, Debug)]
pub struct TabModel {
    tabs: Vec<Tab>,
    active: TabId,
    next_id: u64,
}

/// Per-page vertical scroll state. The browser keeps one instance alongside
/// each tab's retained display list, so activation does not reset position.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct PageScrollState {
    offset_y: f32,
    content_height: f32,
    viewport_height: f32,
}

impl PageScrollState {
    #[must_use]
    pub const fn offset_y(self) -> f32 {
        self.offset_y
    }

    #[must_use]
    pub const fn content_height(self) -> f32 {
        self.content_height
    }

    #[must_use]
    pub const fn viewport_height(self) -> f32 {
        self.viewport_height
    }

    #[must_use]
    pub fn max_offset_y(self) -> f32 {
        (self.content_height - self.viewport_height).max(0.0)
    }

    /// Install new layout metrics and clamp, without otherwise resetting the
    /// position (for example, when a tab is resized or reactivated).
    pub fn update_metrics(&mut self, content_height: f32, viewport_height: f32) {
        self.content_height = finite_non_negative(content_height);
        self.viewport_height = finite_non_negative(viewport_height);
        self.offset_y = self.offset_y.min(self.max_offset_y());
    }

    /// Apply a document-space delta and report whether painting must change.
    pub fn scroll_by(&mut self, delta_y: f32) -> bool {
        if !delta_y.is_finite() {
            return false;
        }
        let previous = self.offset_y;
        self.offset_y = (self.offset_y + delta_y).clamp(0.0, self.max_offset_y());
        self.offset_y.to_bits() != previous.to_bits()
    }

    /// Reset on a committed navigation to a new document.
    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

fn finite_non_negative(value: f32) -> f32 {
    if value.is_finite() {
        value.max(0.0)
    } else {
        0.0
    }
}

impl TabModel {
    #[must_use]
    pub fn new(title: impl Into<String>, address: impl Into<String>) -> Self {
        let first = Tab {
            id: TabId(1),
            title: title.into(),
            address: address.into(),
            loading: false,
        };
        Self {
            tabs: vec![first],
            active: TabId(1),
            next_id: 2,
        }
    }

    #[must_use]
    pub fn tabs(&self) -> &[Tab] {
        &self.tabs
    }

    #[must_use]
    pub const fn active_id(&self) -> TabId {
        self.active
    }

    #[must_use]
    /// Return the active tab.
    ///
    /// # Panics
    ///
    /// Panics only if the model invariant is broken internally and the active
    /// identifier no longer names one of the non-empty tab list entries.
    pub fn active(&self) -> &Tab {
        self.tabs
            .iter()
            .find(|tab| tab.id == self.active)
            .expect("a tab model always has an active tab")
    }

    /// Return the active tab mutably.
    ///
    /// # Panics
    ///
    /// Panics only if the model invariant is broken internally and the active
    /// identifier no longer names one of the non-empty tab list entries.
    pub fn active_mut(&mut self) -> &mut Tab {
        self.tabs
            .iter_mut()
            .find(|tab| tab.id == self.active)
            .expect("a tab model always has an active tab")
    }

    pub fn apply(&mut self, intent: TabIntent) -> Option<TabId> {
        match intent {
            TabIntent::New => Some(self.new_home_tab()),
            TabIntent::Close(tab) => self.close(tab),
            TabIntent::Activate(tab) => {
                if self.tabs.iter().any(|candidate| candidate.id == tab) {
                    self.active = tab;
                }
                None
            }
            TabIntent::Move { tab, index } => {
                self.move_to(tab, index);
                None
            }
        }
    }

    pub fn update(&mut self, id: TabId, title: impl Into<String>, address: impl Into<String>) {
        if let Some(tab) = self.tabs.iter_mut().find(|tab| tab.id == id) {
            tab.title = title.into();
            tab.address = address.into();
        }
    }

    pub fn set_loading(&mut self, id: TabId, loading: bool) {
        if let Some(tab) = self.tabs.iter_mut().find(|tab| tab.id == id) {
            tab.loading = loading;
        }
    }

    fn new_home_tab(&mut self) -> TabId {
        let id = TabId(self.next_id);
        self.next_id = self.next_id.saturating_add(1);
        self.tabs.push(Tab {
            id,
            title: "New tab".to_owned(),
            address: "about:home".to_owned(),
            loading: false,
        });
        self.active = id;
        id
    }

    fn close(&mut self, id: TabId) -> Option<TabId> {
        let index = self.tabs.iter().position(|tab| tab.id == id)?;
        let was_active = self.active == id;
        self.tabs.remove(index);
        if self.tabs.is_empty() {
            return Some(self.new_home_tab());
        }
        if was_active {
            self.active = self.tabs[index.min(self.tabs.len() - 1)].id;
        }
        None
    }

    fn move_to(&mut self, id: TabId, index: usize) {
        let Some(old_index) = self.tabs.iter().position(|tab| tab.id == id) else {
            return;
        };
        let tab = self.tabs.remove(old_index);
        self.tabs.insert(index.min(self.tabs.len()), tab);
    }
}

#[cfg(test)]
mod tests {
    use super::{PageScrollState, TabIntent, TabModel};

    #[test]
    fn page_scroll_state_clamps_and_tabs_can_keep_independent_offsets() {
        let mut first = PageScrollState::default();
        let mut second = PageScrollState::default();
        first.update_metrics(1_000.0, 300.0);
        second.update_metrics(500.0, 300.0);

        assert!(first.scroll_by(800.0));
        assert!(second.scroll_by(75.0));
        assert!((first.offset_y() - 700.0).abs() < f32::EPSILON);
        assert!((second.offset_y() - 75.0).abs() < f32::EPSILON);
        assert!(!first.scroll_by(1.0));

        first.reset();
        assert!(first.offset_y().abs() < f32::EPSILON);
        assert!((second.offset_y() - 75.0).abs() < f32::EPSILON);
    }

    #[test]
    fn closing_last_tab_creates_a_fresh_home_tab() {
        let mut tabs = TabModel::new("First", "about:home");
        let old = tabs.active_id();
        let created = tabs.apply(TabIntent::Close(old));
        assert_eq!(tabs.tabs().len(), 1);
        assert_ne!(tabs.active_id(), old);
        assert_eq!(created, Some(tabs.active_id()));
    }

    #[test]
    fn close_selects_the_neighbor_of_active_tab() {
        let mut tabs = TabModel::new("First", "about:home");
        let first = tabs.active_id();
        let second = tabs.apply(TabIntent::New).expect("new tab id");
        tabs.apply(TabIntent::Close(second));
        assert_eq!(tabs.active_id(), first);
    }

    #[test]
    fn tabs_can_be_reordered_without_changing_identity() {
        let mut tabs = TabModel::new("First", "about:home");
        let first = tabs.active_id();
        let second = tabs.apply(TabIntent::New).expect("new tab id");
        let third = tabs.apply(TabIntent::New).expect("new tab id");
        tabs.apply(TabIntent::Move {
            tab: third,
            index: 0,
        });
        assert_eq!(
            tabs.tabs().iter().map(|tab| tab.id).collect::<Vec<_>>(),
            [third, first, second]
        );
    }
}
