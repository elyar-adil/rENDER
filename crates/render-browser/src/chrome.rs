//! Layout, hit-testing, and deterministic CPU painting for browser chrome.
#![allow(
    clippy::cast_precision_loss,
    reason = "native window coordinates are bounded far below f32's exact integer range"
)]

use crate::editor::{AddressCommand, AddressEditor};
use crate::model::{Tab, TabId, TabIntent};
use crate::navigation::NavigationIntent;
use std::time::Duration;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Point {
    pub x: f32,
    pub y: f32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl Rect {
    #[must_use]
    pub fn contains(self, point: Point) -> bool {
        point.x >= self.x
            && point.y >= self.y
            && point.x < self.x + self.width
            && point.y < self.y + self.height
    }

    #[must_use]
    pub const fn inset(self, amount: f32) -> Self {
        Self {
            x: self.x + amount,
            y: self.y + amount,
            width: self.width - amount * 2.0,
            height: self.height - amount * 2.0,
        }
    }

    #[must_use]
    pub fn contains_top_rounded(self, point: Point, radius: f32) -> bool {
        if !self.contains(point) {
            return false;
        }
        let radius = radius.min(self.width * 0.5).min(self.height * 0.5);
        if radius <= 0.0 || point.y >= self.y + radius {
            return true;
        }
        let corner_x = if point.x < self.x + radius {
            self.x + radius
        } else if point.x >= self.x + self.width - radius {
            self.x + self.width - radius
        } else {
            return true;
        };
        let corner_y = self.y + radius;
        let dx = point.x - corner_x;
        let dy = point.y - corner_y;
        dx.mul_add(dx, dy * dy) <= radius * radius
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolbarButton {
    Back,
    Forward,
    Reload,
    Home,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WindowControl {
    Minimize,
    MaximizeRestore,
    Close,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WindowAction {
    Minimize,
    ToggleMaximize,
    Close,
}

impl WindowControl {
    #[must_use]
    pub const fn action(self) -> WindowAction {
        match self {
            Self::Minimize => WindowAction::Minimize,
            Self::MaximizeRestore => WindowAction::ToggleMaximize,
            Self::Close => WindowAction::Close,
        }
    }
}

impl ToolbarButton {
    #[must_use]
    pub const fn navigation_intent(self) -> NavigationIntent {
        match self {
            Self::Back => NavigationIntent::Back,
            Self::Forward => NavigationIntent::Forward,
            Self::Reload => NavigationIntent::Reload,
            Self::Home => NavigationIntent::Home,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HitTarget {
    Tab(TabId),
    CloseTab(TabId),
    NewTab,
    Toolbar(ToolbarButton),
    WindowControl(WindowControl),
    AddressBar,
    TitleBar,
    Content,
    Chrome,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TabGeometry {
    pub id: TabId,
    pub bounds: Rect,
    pub close: Rect,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ButtonGeometry {
    pub button: ToolbarButton,
    pub bounds: Rect,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WindowControlGeometry {
    pub control: WindowControl,
    pub bounds: Rect,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ChromeLayout {
    pub tabs: Vec<TabGeometry>,
    pub new_tab: Rect,
    pub window_controls: Vec<WindowControlGeometry>,
    pub title_bar: Rect,
    pub buttons: Vec<ButtonGeometry>,
    pub address: Rect,
    pub content: Rect,
    pub chrome_height: u32,
    pub scale: f32,
}

impl ChromeLayout {
    #[must_use]
    pub fn new(width: u32, height: u32, scale: f32, tabs: &[Tab]) -> Self {
        Self::new_with_decorations(width, height, scale, tabs, false)
    }

    /// Layout used by the live browser window when the OS owns the title bar
    /// and its minimize/maximize/close controls. The client area still draws
    /// browser tabs, navigation, and the address editor, but never reserves a
    /// second synthetic title bar.
    #[must_use]
    pub fn new_native(width: u32, height: u32, scale: f32, tabs: &[Tab]) -> Self {
        // Kept as a source-compatible alias for callers that used the old
        // native-frame layout. The browser now owns a modern, cross-platform
        // title row so tabs and window actions stay together on every OS.
        Self::new(width, height, scale, tabs)
    }

    #[must_use]
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::too_many_lines,
        reason = "native dimensions and DPI-scaled chrome metrics are finite positive pixels"
    )]
    fn new_with_decorations(
        width: u32,
        height: u32,
        scale: f32,
        tabs: &[Tab],
        native_decorations: bool,
    ) -> Self {
        let scale = scale.max(0.5);
        let width_f = width as f32;
        let tab_strip_height = 42.0 * scale;
        let toolbar_height = 54.0 * scale;
        let chrome_height = (tab_strip_height + toolbar_height).round() as u32;
        let left = 12.0 * scale;
        let tab_gap = 4.0 * scale;
        let control_width = 46.0 * scale;
        let control_kinds = [
            WindowControl::Minimize,
            WindowControl::MaximizeRestore,
            WindowControl::Close,
        ];
        let control_group_width = control_width * control_kinds.len() as f32;
        // With native decorations the platform title bar is outside the
        // client area. Keep all client pixels available to the tab strip and
        // do not create a second set of synthetic window buttons.
        let controls_x = if native_decorations {
            width_f
        } else {
            (width_f - control_group_width).max(0.0)
        };
        let window_controls = if native_decorations {
            Vec::new()
        } else {
            control_kinds
                .into_iter()
                .enumerate()
                .map(|(index, control)| WindowControlGeometry {
                    control,
                    bounds: Rect {
                        x: controls_x + index as f32 * control_width,
                        y: 0.0,
                        width: control_width,
                        height: tab_strip_height,
                    },
                })
                .collect::<Vec<_>>()
        };
        let new_tab_width = 30.0 * scale;
        let tab_area_right = (controls_x - new_tab_width - 8.0 * scale).max(left);
        let tab_area = (tab_area_right - left).max(0.0);
        #[allow(
            clippy::cast_precision_loss,
            reason = "tab count is bounded by available UI space"
        )]
        let count = tabs.len().max(1) as f32;
        let tab_width = ((tab_area - tab_gap * (count - 1.0)).max(0.0) / count).min(230.0 * scale);
        let tab_height = 34.0 * scale;
        let tab_y = tab_strip_height - tab_height;
        let geometries = tabs
            .iter()
            .enumerate()
            .map(|(index, tab)| {
                #[allow(
                    clippy::cast_precision_loss,
                    reason = "tab count is bounded by UI state"
                )]
                let index = index as f32;
                let x = left + index * (tab_width + tab_gap);
                let bounds = Rect {
                    x,
                    y: tab_y,
                    width: tab_width,
                    height: tab_height,
                };
                let close_size = (24.0 * scale).min(tab_width);
                let close_inset = (5.0 * scale).min((tab_width - close_size).max(0.0));
                TabGeometry {
                    id: tab.id,
                    bounds,
                    close: Rect {
                        x: x + tab_width - close_size - close_inset,
                        y: tab_y + (tab_height - close_size) * 0.5,
                        width: close_size,
                        height: close_size,
                    },
                }
            })
            .collect::<Vec<_>>();
        let tabs_end = geometries
            .last()
            .map_or(left, |tab| tab.bounds.x + tab.bounds.width + tab_gap);
        let new_tab = Rect {
            x: tabs_end,
            y: tab_y + 4.0 * scale,
            width: new_tab_width,
            height: 28.0 * scale,
        };
        let title_bar_x = new_tab.x + new_tab.width + 4.0 * scale;
        let title_bar = Rect {
            x: title_bar_x,
            y: 0.0,
            width: if native_decorations {
                0.0
            } else {
                (controls_x - title_bar_x).max(0.0)
            },
            height: tab_strip_height,
        };

        let toolbar_y = tab_strip_height;
        let button_size = 36.0 * scale;
        let button_gap = 3.0 * scale;
        let button_y = toolbar_y + (toolbar_height - button_size) * 0.5;
        let button_kinds = [
            ToolbarButton::Back,
            ToolbarButton::Forward,
            ToolbarButton::Reload,
            ToolbarButton::Home,
        ];
        let buttons = button_kinds
            .into_iter()
            .enumerate()
            .map(|(index, button)| {
                #[allow(clippy::cast_precision_loss, reason = "there are exactly four buttons")]
                let index = index as f32;
                ButtonGeometry {
                    button,
                    bounds: Rect {
                        x: left + index * (button_size + button_gap),
                        y: button_y,
                        width: button_size,
                        height: button_size,
                    },
                }
            })
            .collect::<Vec<_>>();
        let address_x = left + 4.0 * (button_size + button_gap) + 7.0 * scale;
        let address = Rect {
            x: address_x,
            y: button_y,
            width: (width_f - address_x - 14.0 * scale).max(40.0 * scale),
            height: button_size,
        };
        let content_y = chrome_height.min(height);
        Self {
            tabs: geometries,
            new_tab,
            window_controls,
            title_bar,
            buttons,
            address,
            content: Rect {
                x: 0.0,
                y: content_y as f32,
                width: width_f,
                height: height.saturating_sub(content_y) as f32,
            },
            chrome_height,
            scale,
        }
    }

    #[must_use]
    pub fn hit_test(&self, point: Point) -> HitTarget {
        for tab in &self.tabs {
            if tab.close.contains(point) {
                return HitTarget::CloseTab(tab.id);
            }
            if tab.bounds.contains_top_rounded(point, 9.0 * self.scale) {
                return HitTarget::Tab(tab.id);
            }
        }
        if self.new_tab.contains(point) {
            return HitTarget::NewTab;
        }
        for control in &self.window_controls {
            if control.bounds.contains(point) {
                return HitTarget::WindowControl(control.control);
            }
        }
        if self.title_bar.contains(point) {
            return HitTarget::TitleBar;
        }
        for button in &self.buttons {
            if button.bounds.contains(point) {
                return HitTarget::Toolbar(button.button);
            }
        }
        if self.address.contains(point) {
            return HitTarget::AddressBar;
        }
        if self.content.contains(point) {
            HitTarget::Content
        } else {
            HitTarget::Chrome
        }
    }

    #[must_use]
    pub fn reorder_index(&self, dragged: TabId, pointer_x: f32) -> Option<usize> {
        let current = self.tabs.iter().position(|tab| tab.id == dragged)?;
        let mut index = self.tabs.len().saturating_sub(1);
        for (candidate, geometry) in self.tabs.iter().enumerate() {
            if pointer_x < geometry.bounds.x + geometry.bounds.width * 0.5 {
                index = candidate;
                break;
            }
        }
        (index != current).then_some(index)
    }

    /// Static slot stride (uniform tab width plus gap) used by drag geometry.
    fn drag_stride(&self) -> Option<(f32, f32, f32, f32)> {
        let first = self.tabs.first()?;
        let width = first.bounds.width;
        let stride = self
            .tabs
            .get(1)
            .map_or(0.0, |second| second.bounds.x - first.bounds.x);
        Some((first.bounds.x, width, stride, stride - width))
    }

    /// Per-tab paint offsets for one live-drag frame.
    ///
    /// The dragged tab is painted at `dragged_x` (clamped to the strip);
    /// every other tab slides one slot toward the dragged tab's origin, with
    /// the slide progressing across the gap the dragged tab's center is
    /// currently crossing. At the extremes this reproduces the exact slot
    /// order the committed reorder will produce.
    #[must_use]
    fn drag_slot_offsets(&self, dragged: TabId, dragged_x: f32) -> Vec<f32> {
        let Some(origin) = self.tabs.iter().position(|tab| tab.id == dragged) else {
            return Vec::new();
        };
        let Some((base, width, stride, gap)) = self.drag_stride() else {
            return Vec::new();
        };
        let count = self.tabs.len();
        let center = dragged_x.clamp(base, base + (count - 1) as f32 * stride) + width * 0.5;
        self.tabs
            .iter()
            .enumerate()
            .map(|(index, geometry)| {
                if index == origin {
                    return center - width * 0.5 - geometry.bounds.x;
                }
                // The midpoint between this slot and the adjacent slot on the
                // side facing the dragged tab's origin.
                let midpoint = if index > origin {
                    base + index as f32 * stride - gap * 0.5
                } else {
                    base + (index + 1) as f32 * stride - gap * 0.5
                };
                let progress = if index > origin {
                    ((center - midpoint) / stride + 0.5).clamp(0.0, 1.0)
                } else {
                    ((midpoint - center) / stride + 0.5).clamp(0.0, 1.0)
                };
                let shift = stride * progress;
                if index > origin { -shift } else { shift }
            })
            .collect()
    }

    /// The slot index the dragged tab would occupy if released at `dragged_x`.
    #[must_use]
    fn drag_insertion_index(&self, dragged: TabId, dragged_x: f32) -> Option<usize> {
        let origin = self.tabs.iter().position(|tab| tab.id == dragged)?;
        let (base, width, stride, _) = self.drag_stride()?;
        let count = self.tabs.len();
        let center = dragged_x.clamp(base, base + (count - 1) as f32 * stride) + width * 0.5;
        let nearest = ((center - base - width * 0.5) / stride).round();
        if !nearest.is_finite() {
            return Some(origin);
        }
        #[allow(
            clippy::cast_possible_truncation,
            reason = "the rounded slot index is bounded by the tab count before the cast"
        )]
        let index = usize::try_from(nearest as i64).unwrap_or(origin);
        Some(index.min(count - 1))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TitleBarGesture {
    BeginDrag,
    ToggleMaximize,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct TitleBarClickTracker {
    previous: Option<(Duration, Point)>,
}

impl TitleBarClickTracker {
    const DOUBLE_CLICK_INTERVAL: Duration = Duration::from_millis(500);

    #[must_use]
    pub fn register(&mut self, timestamp: Duration, point: Point, scale: f32) -> TitleBarGesture {
        let movement_limit = 5.0 * scale.max(0.5);
        let is_double_click = self
            .previous
            .is_some_and(|(previous_time, previous_point)| {
                let elapsed = timestamp.saturating_sub(previous_time);
                let dx = point.x - previous_point.x;
                let dy = point.y - previous_point.y;
                elapsed <= Self::DOUBLE_CLICK_INTERVAL
                    && dx.mul_add(dx, dy * dy) <= movement_limit * movement_limit
            });
        if is_double_click {
            self.previous = None;
            TitleBarGesture::ToggleMaximize
        } else {
            self.previous = Some((timestamp, point));
            TitleBarGesture::BeginDrag
        }
    }

    pub fn reset(&mut self) {
        self.previous = None;
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TabDrag {
    tab: TabId,
    start_x: f32,
    active: bool,
    /// Pointer offset from the dragged tab's static left edge, captured when
    /// the drag activates so the tab keeps following the grab point.
    grab_offset: f32,
}

impl TabDrag {
    #[must_use]
    pub const fn new(tab: TabId, start_x: f32) -> Self {
        Self {
            tab,
            start_x,
            active: false,
            grab_offset: 0.0,
        }
    }

    /// The tab being dragged.
    #[must_use]
    pub const fn tab(&self) -> TabId {
        self.tab
    }

    /// Paint offsets (one per tab, in [`ChromeLayout::tabs`] order) for a live
    /// drag: the dragged tab follows the pointer continuously while neighbors
    /// slide toward the vacated slot in proportion to how far the dragged
    /// tab's center has crossed each gap midpoint. Returns `None` until the
    /// drag crosses its activation threshold.
    #[must_use]
    pub fn paint_offsets(&mut self, pointer_x: f32, layout: &ChromeLayout) -> Option<Vec<f32>> {
        self.activate(pointer_x, layout);
        if !self.active {
            return None;
        }
        Some(layout.drag_slot_offsets(self.tab, pointer_x - self.grab_offset))
    }

    /// The reorder committed when the pointer is released after a live drag.
    #[must_use]
    pub fn release(&mut self, pointer_x: f32, layout: &ChromeLayout) -> Option<TabIntent> {
        self.activate(pointer_x, layout);
        if !self.active {
            return None;
        }
        layout
            .drag_insertion_index(self.tab, pointer_x - self.grab_offset)
            .map(|index| TabIntent::Move {
                tab: self.tab,
                index,
            })
    }

    fn activate(&mut self, pointer_x: f32, layout: &ChromeLayout) {
        if self.active || (pointer_x - self.start_x).abs() < 6.0 * layout.scale {
            return;
        }
        self.active = true;
        // The grab offset comes from the pointer-down position so the tab
        // tracks the grab point from the first activated frame on.
        self.grab_offset = self.start_x
            - layout
                .tabs
                .iter()
                .find(|geometry| geometry.id == self.tab)
                .map_or(0.0, |geometry| geometry.bounds.x);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChromeTheme {
    Light,
    Dark,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct AddressClickTracker {
    previous: Option<(Duration, Point)>,
}

impl AddressClickTracker {
    const DOUBLE_CLICK_INTERVAL: Duration = Duration::from_millis(500);

    /// Returns true for the second click of a spatially close double click.
    pub fn register(&mut self, timestamp: Duration, point: Point, scale: f32) -> bool {
        let movement_limit = 5.0 * scale.max(0.5);
        let is_double_click = self
            .previous
            .is_some_and(|(previous_time, previous_point)| {
                let elapsed = timestamp.saturating_sub(previous_time);
                let dx = point.x - previous_point.x;
                let dy = point.y - previous_point.y;
                elapsed <= Self::DOUBLE_CLICK_INTERVAL
                    && dx.mul_add(dx, dy * dy) <= movement_limit * movement_limit
            });
        self.previous = (!is_double_click).then_some((timestamp, point));
        is_double_click
    }

    pub fn reset(&mut self) {
        self.previous = None;
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AddressMenuItem {
    pub command: AddressCommand,
    pub bounds: Rect,
    pub enabled: bool,
}

/// DPI-scaled, viewport-clamped geometry for the self-painted address menu.
#[derive(Clone, Debug, PartialEq)]
pub struct AddressContextMenu {
    pub bounds: Rect,
    pub items: Vec<AddressMenuItem>,
    pub scale: f32,
}

impl AddressContextMenu {
    #[must_use]
    pub fn new(
        origin: Point,
        viewport_width: u32,
        viewport_height: u32,
        scale: f32,
        editor: &AddressEditor,
        paste_available: bool,
    ) -> Self {
        let scale = scale.max(0.5);
        let width = 204.0 * scale;
        let row_height = 30.0 * scale;
        let padding = 6.0 * scale;
        let separator_gap = 5.0 * scale;
        let height = padding * 2.0 + row_height * 6.0 + separator_gap * 2.0;
        let viewport_width = viewport_width as f32;
        let viewport_height = viewport_height as f32;
        let bounds = Rect {
            x: origin.x.max(0.0).min((viewport_width - width).max(0.0)),
            y: origin.y.max(0.0).min((viewport_height - height).max(0.0)),
            width: width.min(viewport_width),
            height: height.min(viewport_height),
        };
        let commands = [
            AddressCommand::Undo,
            AddressCommand::Cut,
            AddressCommand::Copy,
            AddressCommand::Paste,
            AddressCommand::Delete,
            AddressCommand::SelectAll,
        ];
        let mut y = bounds.y + padding;
        let items = commands
            .into_iter()
            .enumerate()
            .map(|(index, command)| {
                let item = AddressMenuItem {
                    command,
                    bounds: Rect {
                        x: bounds.x + padding,
                        y,
                        width: (bounds.width - padding * 2.0).max(0.0),
                        height: row_height.min((bounds.y + bounds.height - y).max(0.0)),
                    },
                    enabled: editor.command_is_enabled(command, paste_available),
                };
                y += row_height;
                if index == 0 || index == 4 {
                    y += separator_gap;
                }
                item
            })
            .collect();
        Self {
            bounds,
            items,
            scale,
        }
    }

    #[must_use]
    pub fn contains(&self, point: Point) -> bool {
        self.bounds.contains(point)
    }

    #[must_use]
    pub fn item_at(&self, point: Point) -> Option<AddressMenuItem> {
        self.items
            .iter()
            .copied()
            .find(|item| item.bounds.contains(point))
    }
}

pub trait TextPainter {
    fn measure(&self, text: &str, size: f32) -> f32;
    fn paint(&self, canvas: &mut Canvas<'_>, text: &str, origin: Point, size: f32, color: u32);
}

pub struct Canvas<'a> {
    pixels: &'a mut [u32],
    width: u32,
    height: u32,
    clip: Rect,
}

impl<'a> Canvas<'a> {
    pub fn new(pixels: &'a mut [u32], width: u32, height: u32) -> Self {
        debug_assert_eq!(pixels.len(), width as usize * height as usize);
        Self {
            pixels,
            width,
            height,
            clip: Rect {
                x: 0.0,
                y: 0.0,
                width: width as f32,
                height: height as f32,
            },
        }
    }

    pub fn with_clip<R>(&mut self, rect: Rect, paint: impl FnOnce(&mut Self) -> R) -> R {
        let previous = self.clip;
        let left = previous.x.max(rect.x);
        let top = previous.y.max(rect.y);
        let right = (previous.x + previous.width).min(rect.x + rect.width);
        let bottom = (previous.y + previous.height).min(rect.y + rect.height);
        self.clip = Rect {
            x: left,
            y: top,
            width: (right - left).max(0.0),
            height: (bottom - top).max(0.0),
        };
        let result = paint(self);
        self.clip = previous;
        result
    }

    pub fn fill(&mut self, color: u32) {
        self.pixels.fill(color);
    }

    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "rectangles are clipped to finite framebuffer bounds before conversion"
    )]
    pub fn rect(&mut self, rect: Rect, color: u32) {
        let left = rect.x.max(self.clip.x).floor().max(0.0) as u32;
        let top = rect.y.max(self.clip.y).floor().max(0.0) as u32;
        let right = (rect.x + rect.width)
            .min(self.clip.x + self.clip.width)
            .ceil()
            .clamp(0.0, self.width as f32) as u32;
        let right = right.max(left);
        let bottom = (rect.y + rect.height)
            .min(self.clip.y + self.clip.height)
            .ceil()
            .clamp(0.0, self.height as f32) as u32;
        let bottom = bottom.max(top);
        for y in top..bottom {
            let start = y as usize * self.width as usize + left as usize;
            let end = y as usize * self.width as usize + right as usize;
            self.pixels[start..end].fill(color);
        }
    }

    /// Fill a clipped rectangle with a translucent color while preserving the
    /// pixels underneath. Used by focused page-control selection overlays.
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "rectangles are clipped to finite framebuffer bounds before conversion"
    )]
    pub fn blend_rect(&mut self, rect: Rect, color: u32, alpha: u8) {
        let left = rect.x.max(self.clip.x).floor().max(0.0) as u32;
        let top = rect.y.max(self.clip.y).floor().max(0.0) as u32;
        let right = (rect.x + rect.width)
            .min(self.clip.x + self.clip.width)
            .ceil()
            .clamp(0.0, self.width as f32) as u32;
        let right = right.max(left);
        let bottom = (rect.y + rect.height)
            .min(self.clip.y + self.clip.height)
            .ceil()
            .clamp(0.0, self.height as f32) as u32;
        let bottom = bottom.max(top);
        for y in top..bottom {
            for x in left..right {
                let index = y as usize * self.width as usize + x as usize;
                self.pixels[index] = blend(self.pixels[index], color, alpha);
            }
        }
    }

    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "rounded rectangles are clipped to finite framebuffer bounds"
    )]
    pub fn rounded_rect(&mut self, rect: Rect, radius: f32, color: u32) {
        let left = rect.x.max(self.clip.x).floor().max(0.0) as u32;
        let top = rect.y.max(self.clip.y).floor().max(0.0) as u32;
        let right = (rect.x + rect.width)
            .min(self.clip.x + self.clip.width)
            .ceil()
            .clamp(0.0, self.width as f32) as u32;
        let right = right.max(left);
        let bottom = (rect.y + rect.height)
            .min(self.clip.y + self.clip.height)
            .ceil()
            .clamp(0.0, self.height as f32) as u32;
        let bottom = bottom.max(top);
        let radius = radius.min(rect.width * 0.5).min(rect.height * 0.5);
        for y in top..bottom {
            for x in left..right {
                let px = x as f32 + 0.5;
                let py = y as f32 + 0.5;
                // At fractional DPI scales, two mathematically equal bounds
                // can round in opposite directions (for example 54.0 and
                // 53.999996). `f32::clamp` panics when min > max, so collapse
                // that sub-pixel interval to its midpoint explicitly.
                let nearest_x = clamp_interval(px, rect.x + radius, rect.x + rect.width - radius);
                let nearest_y = clamp_interval(py, rect.y + radius, rect.y + rect.height - radius);
                let dx = px - nearest_x;
                let dy = py - nearest_y;
                if dx.mul_add(dx, dy * dy) <= radius * radius {
                    self.pixels[y as usize * self.width as usize + x as usize] = color;
                }
            }
        }
    }

    pub fn top_rounded_rect(&mut self, rect: Rect, radius: f32, color: u32) {
        let radius = radius.min(rect.width * 0.5).min(rect.height * 0.5);
        self.rounded_rect(rect, radius, color);
        self.rect(
            Rect {
                x: rect.x,
                y: rect.y + radius,
                width: rect.width,
                height: (rect.height - radius).max(0.0),
            },
            color,
        );
    }

    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "line endpoints are finite DPI-scaled chrome coordinates"
    )]
    pub fn line(&mut self, from: Point, to: Point, thickness: f32, color: u32) {
        let dx = to.x - from.x;
        let dy = to.y - from.y;
        let steps = dx.abs().max(dy.abs()).ceil().max(1.0) as u32;
        for step in 0..=steps {
            let t = step as f32 / steps as f32;
            self.rounded_rect(
                Rect {
                    x: dx.mul_add(t, from.x) - thickness * 0.5,
                    y: dy.mul_add(t, from.y) - thickness * 0.5,
                    width: thickness,
                    height: thickness,
                },
                thickness * 0.5,
                color,
            );
        }
    }

    pub fn blend_mask(&mut self, x: i32, y: i32, width: u32, mask: &[u8], color: u32) {
        for (index, coverage) in mask.iter().copied().enumerate() {
            let glyph_x = i32::try_from(index % width as usize).unwrap_or(0) + x;
            let glyph_y = i32::try_from(index / width as usize).unwrap_or(0) + y;
            if glyph_x < 0
                || glyph_y < 0
                || glyph_x >= i32::try_from(self.width).unwrap_or(i32::MAX)
                || glyph_y >= i32::try_from(self.height).unwrap_or(i32::MAX)
                || (glyph_x as f32) < self.clip.x
                || (glyph_y as f32) < self.clip.y
                || (glyph_x as f32) >= self.clip.x + self.clip.width
                || (glyph_y as f32) >= self.clip.y + self.clip.height
            {
                continue;
            }
            let pixel_index = usize::try_from(glyph_y).unwrap_or(0) * self.width as usize
                + usize::try_from(glyph_x).unwrap_or(0);
            self.pixels[pixel_index] = blend(self.pixels[pixel_index], color, coverage);
        }
    }
}

fn clamp_interval(value: f32, first: f32, second: f32) -> f32 {
    let (lower, upper) = if first <= second {
        (first, second)
    } else {
        let midpoint = (first + second) * 0.5;
        (midpoint, midpoint)
    };
    value.max(lower).min(upper)
}

#[allow(
    clippy::too_many_arguments,
    reason = "chrome painting consumes the complete immutable UI snapshot"
)]
pub fn paint_chrome(
    canvas: &mut Canvas<'_>,
    layout: &ChromeLayout,
    tabs: &[Tab],
    active: TabId,
    editor: &AddressEditor,
    theme: ChromeTheme,
    hot: HitTarget,
    maximized: bool,
    drag: Option<(TabId, &[f32])>,
    text: &impl TextPainter,
) {
    let palette = Palette::new(theme);
    canvas.rect(
        Rect {
            x: 0.0,
            y: 0.0,
            width: layout.content.width,
            height: layout.chrome_height as f32,
        },
        palette.tab_strip,
    );
    let toolbar_top = layout
        .buttons
        .first()
        .map_or(0.0, |button| button.bounds.y - 9.0 * layout.scale);
    canvas.rect(
        Rect {
            x: 0.0,
            y: toolbar_top,
            width: layout.content.width,
            height: layout.chrome_height as f32 - toolbar_top,
        },
        palette.toolbar,
    );
    paint_tabs(canvas, layout, tabs, active, hot, drag, text, palette);
    paint_window_controls(canvas, layout, hot, maximized, palette);
    paint_toolbar(canvas, layout, hot, palette);
    paint_address(canvas, layout, editor, text, palette);
}

/// Overlay page scrollbar: a translucent track strip with a rounded thumb.
/// `hot`/`dragging` darken the thumb the way mainstream browsers do.
pub fn paint_scrollbar(
    canvas: &mut Canvas<'_>,
    geometry: &ScrollbarGeometry,
    hot: bool,
    dragging: bool,
    theme: ChromeTheme,
) {
    let (resting, active_color) = match theme {
        ChromeTheme::Light => (0x00c1_c4c9, 0x00a8_acb2),
        ChromeTheme::Dark => (0x004a_4d55, 0x005c_6068),
    };
    let color = if dragging || hot {
        active_color
    } else {
        resting
    };
    canvas.rounded_rect(geometry.thumb, geometry.thumb.width * 0.5, color);
}

#[allow(
    clippy::too_many_arguments,
    reason = "painting a tab strip with live drag state needs the full visual snapshot"
)]
fn paint_tabs(
    canvas: &mut Canvas<'_>,
    layout: &ChromeLayout,
    tabs: &[Tab],
    active: TabId,
    hot: HitTarget,
    drag: Option<(TabId, &[f32])>,
    text: &impl TextPainter,
    palette: Palette,
) {
    let Some((dragged, drag_offsets)) = drag else {
        for (tab, geometry) in tabs.iter().zip(&layout.tabs) {
            paint_one_tab(
                canvas, layout, tab, *geometry, 0.0, active, hot, false, text, palette,
            );
        }
        paint_new_tab_button(canvas, layout, hot, palette);
        return;
    };
    let offsets = |index: usize| drag_offsets.get(index).copied().unwrap_or(0.0);
    for (index, (tab, geometry)) in tabs.iter().zip(&layout.tabs).enumerate() {
        if tab.id == dragged {
            continue;
        }
        paint_one_tab(
            canvas,
            layout,
            tab,
            *geometry,
            offsets(index),
            active,
            hot,
            true,
            text,
            palette,
        );
    }
    if let Some((index, (tab, geometry))) = tabs
        .iter()
        .zip(&layout.tabs)
        .enumerate()
        .find(|(_, (tab, _))| tab.id == dragged)
    {
        paint_one_tab(
            canvas,
            layout,
            tab,
            *geometry,
            offsets(index),
            active,
            hot,
            true,
            text,
            palette,
        );
    }
    paint_new_tab_button(canvas, layout, hot, palette);
}

#[allow(
    clippy::too_many_arguments,
    reason = "one tab's full visual state is naturally wider than a handful of fields"
)]
fn paint_one_tab(
    canvas: &mut Canvas<'_>,
    layout: &ChromeLayout,
    tab: &Tab,
    geometry: TabGeometry,
    offset: f32,
    active: TabId,
    hot: HitTarget,
    dragging: bool,
    text: &impl TextPainter,
    palette: Palette,
) {
    let geometry = translate_tab_geometry(geometry, offset);
    let is_active = tab.id == active;
    let is_hot =
        !dragging && matches!(hot, HitTarget::Tab(id) | HitTarget::CloseTab(id) if id == tab.id);
    let background = if is_active {
        palette.toolbar
    } else if is_hot {
        palette.tab_hover
    } else {
        palette.tab_strip
    };
    canvas.top_rounded_rect(geometry.bounds, 9.0 * layout.scale, background);
    let mut text_x = geometry.bounds.x + 14.0 * layout.scale;
    if tab.loading {
        let indicator = 7.0 * layout.scale;
        canvas.rounded_rect(
            Rect {
                x: text_x,
                y: geometry.bounds.y + (geometry.bounds.height - indicator) * 0.5,
                width: indicator,
                height: indicator,
            },
            indicator * 0.5,
            palette.accent,
        );
        text_x += 14.0 * layout.scale;
    }
    let max_width = (geometry.close.x - text_x - 5.0 * layout.scale).max(0.0);
    let title = elide(&tab.title, max_width, 13.0 * layout.scale, text);
    text.paint(
        canvas,
        &title,
        Point {
            x: text_x,
            y: geometry.bounds.y + 9.0 * layout.scale,
        },
        13.0 * layout.scale,
        palette.text,
    );
    paint_close_icon(canvas, geometry.close, layout.scale, palette.icon);
}

fn translate_tab_geometry(geometry: TabGeometry, offset: f32) -> TabGeometry {
    if offset == 0.0 {
        return geometry;
    }
    TabGeometry {
        id: geometry.id,
        bounds: Rect {
            x: geometry.bounds.x + offset,
            ..geometry.bounds
        },
        close: Rect {
            x: geometry.close.x + offset,
            ..geometry.close
        },
    }
}

fn paint_new_tab_button(
    canvas: &mut Canvas<'_>,
    layout: &ChromeLayout,
    hot: HitTarget,
    palette: Palette,
) {
    let plus = layout.new_tab;
    if hot == HitTarget::NewTab {
        canvas.rounded_rect(plus, 7.0 * layout.scale, palette.tab_hover);
    }
    let center = Point {
        x: plus.x + plus.width * 0.5,
        y: plus.y + plus.height * 0.5,
    };
    canvas.line(
        Point {
            x: center.x - 5.0 * layout.scale,
            y: center.y,
        },
        Point {
            x: center.x + 5.0 * layout.scale,
            y: center.y,
        },
        1.5 * layout.scale,
        palette.icon,
    );
    canvas.line(
        Point {
            x: center.x,
            y: center.y - 5.0 * layout.scale,
        },
        Point {
            x: center.x,
            y: center.y + 5.0 * layout.scale,
        },
        1.5 * layout.scale,
        palette.icon,
    );
}

fn paint_window_controls(
    canvas: &mut Canvas<'_>,
    layout: &ChromeLayout,
    hot: HitTarget,
    maximized: bool,
    palette: Palette,
) {
    for geometry in &layout.window_controls {
        let is_hot = hot == HitTarget::WindowControl(geometry.control);
        if is_hot {
            canvas.rect(
                geometry.bounds,
                if geometry.control == WindowControl::Close {
                    palette.close_hover
                } else {
                    palette.button_hover
                },
            );
        }
        let icon_color = if is_hot && geometry.control == WindowControl::Close {
            0x00ff_ffff
        } else {
            palette.icon
        };
        paint_window_control_icon(canvas, *geometry, layout.scale, maximized, icon_color);
    }
}

fn paint_window_control_icon(
    canvas: &mut Canvas<'_>,
    geometry: WindowControlGeometry,
    scale: f32,
    maximized: bool,
    color: u32,
) {
    let center = Point {
        x: geometry.bounds.x + geometry.bounds.width * 0.5,
        y: geometry.bounds.y + geometry.bounds.height * 0.5,
    };
    let line = 1.25 * scale;
    match geometry.control {
        WindowControl::Minimize => canvas.line(
            Point {
                x: center.x - 5.0 * scale,
                y: center.y + 3.0 * scale,
            },
            Point {
                x: center.x + 5.0 * scale,
                y: center.y + 3.0 * scale,
            },
            line,
            color,
        ),
        WindowControl::MaximizeRestore if maximized => {
            let size = 8.0 * scale;
            canvas.rect(
                Rect {
                    x: center.x - size * 0.5 + 2.0 * scale,
                    y: center.y - size * 0.5 - 2.0 * scale,
                    width: size,
                    height: line,
                },
                color,
            );
            paint_outline_rect(
                canvas,
                Rect {
                    x: center.x - size * 0.5 - 2.0 * scale,
                    y: center.y - size * 0.5 + 2.0 * scale,
                    width: size,
                    height: size,
                },
                line,
                color,
            );
        }
        WindowControl::MaximizeRestore => paint_outline_rect(
            canvas,
            Rect {
                x: center.x - 5.0 * scale,
                y: center.y - 5.0 * scale,
                width: 10.0 * scale,
                height: 10.0 * scale,
            },
            line,
            color,
        ),
        WindowControl::Close => {
            let arm = 5.0 * scale;
            canvas.line(
                Point {
                    x: center.x - arm,
                    y: center.y - arm,
                },
                Point {
                    x: center.x + arm,
                    y: center.y + arm,
                },
                line,
                color,
            );
            canvas.line(
                Point {
                    x: center.x + arm,
                    y: center.y - arm,
                },
                Point {
                    x: center.x - arm,
                    y: center.y + arm,
                },
                line,
                color,
            );
        }
    }
}

fn paint_outline_rect(canvas: &mut Canvas<'_>, rect: Rect, thickness: f32, color: u32) {
    canvas.rect(
        Rect {
            height: thickness,
            ..rect
        },
        color,
    );
    canvas.rect(
        Rect {
            y: rect.y + rect.height - thickness,
            height: thickness,
            ..rect
        },
        color,
    );
    canvas.rect(
        Rect {
            width: thickness,
            ..rect
        },
        color,
    );
    canvas.rect(
        Rect {
            x: rect.x + rect.width - thickness,
            width: thickness,
            ..rect
        },
        color,
    );
}

fn paint_toolbar(canvas: &mut Canvas<'_>, layout: &ChromeLayout, hot: HitTarget, palette: Palette) {
    for button in &layout.buttons {
        if hot == HitTarget::Toolbar(button.button) {
            canvas.rounded_rect(button.bounds, 8.0 * layout.scale, palette.button_hover);
        }
        paint_toolbar_icon(canvas, *button, layout.scale, palette.icon);
    }
}

fn paint_address(
    canvas: &mut Canvas<'_>,
    layout: &ChromeLayout,
    editor: &AddressEditor,
    text: &impl TextPainter,
    palette: Palette,
) {
    canvas.rounded_rect(
        layout.address,
        layout.address.height * 0.5,
        if editor.is_focused() {
            palette.address_focused
        } else {
            palette.address
        },
    );
    if editor.is_focused() {
        let border = 1.5 * layout.scale;
        canvas.rounded_rect(layout.address, layout.address.height * 0.5, palette.accent);
        canvas.rounded_rect(
            layout.address.inset(border),
            layout.address.height * 0.5 - border,
            palette.address_focused,
        );
    }
    let clip = layout.address.inset(4.0 * layout.scale);
    canvas.with_clip(clip, |canvas| {
        paint_address_contents(canvas, layout, editor, text, palette);
    });
}

fn paint_address_contents(
    canvas: &mut Canvas<'_>,
    layout: &ChromeLayout,
    editor: &AddressEditor,
    text: &impl TextPainter,
    palette: Palette,
) {
    let geometry = address_text_geometry(layout, editor, text);
    let size = geometry.size;
    let text_x = geometry.base_x;
    let text_y = layout.address.y + 10.0 * layout.scale;
    let cursor_prefix = &editor.text()[..editor.cursor()];
    let cursor_width = text.measure(cursor_prefix, size);
    let offset = geometry.offset;
    if let Some((start, end)) = editor.selection() {
        let prefix = text.measure(&editor.text()[..start], size);
        let selection = text.measure(&editor.text()[start..end], size);
        canvas.rounded_rect(
            Rect {
                x: text_x + offset + prefix,
                y: layout.address.y + 6.0 * layout.scale,
                width: selection,
                height: layout.address.height - 12.0 * layout.scale,
            },
            3.0 * layout.scale,
            palette.selection,
        );
    }
    text.paint(
        canvas,
        editor.text(),
        Point {
            x: text_x + offset,
            y: text_y,
        },
        size,
        palette.text,
    );
    if editor.is_focused() {
        let cursor_x = text_x + offset + cursor_width;
        canvas.rect(
            Rect {
                x: cursor_x,
                y: layout.address.y + 8.0 * layout.scale,
                width: 1.5 * layout.scale,
                height: layout.address.height - 16.0 * layout.scale,
            },
            palette.accent,
        );
        if !editor.preedit().is_empty() {
            text.paint(
                canvas,
                editor.preedit(),
                Point {
                    x: cursor_x,
                    y: text_y,
                },
                size,
                palette.text,
            );
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct AddressTextGeometry {
    base_x: f32,
    offset: f32,
    size: f32,
}

fn address_text_geometry(
    layout: &ChromeLayout,
    editor: &AddressEditor,
    text: &impl TextPainter,
) -> AddressTextGeometry {
    let size = 14.0 * layout.scale;
    let base_x = layout.address.x + 16.0 * layout.scale;
    let available = (layout.address.width - 32.0 * layout.scale).max(0.0);
    let cursor_width = text.measure(&editor.text()[..editor.cursor()], size);
    let full_width = text.measure(editor.text(), size);
    let offset = if cursor_width > available - 8.0 * layout.scale {
        available - cursor_width - 8.0 * layout.scale
    } else if full_width > available {
        0.0
    } else {
        (available - full_width).min(0.0)
    };
    AddressTextGeometry {
        base_x,
        offset,
        size,
    }
}

/// Maps a physical pointer x-coordinate to the nearest UTF-8 character boundary.
/// The calculation shares the exact font metrics and horizontal scrolling used by painting.
#[must_use]
pub fn address_index_at_x(
    layout: &ChromeLayout,
    editor: &AddressEditor,
    pointer_x: f32,
    text: &impl TextPainter,
) -> usize {
    let geometry = address_text_geometry(layout, editor, text);
    let local_x = pointer_x - geometry.base_x - geometry.offset;
    if local_x <= 0.0 {
        return 0;
    }
    let mut previous_offset = 0;
    let mut previous_width = 0.0;
    for (offset, character) in editor.text().char_indices() {
        let next_offset = offset + character.len_utf8();
        let next_width = text.measure(&editor.text()[..next_offset], geometry.size);
        if local_x < (previous_width + next_width) * 0.5 {
            return previous_offset;
        }
        previous_offset = next_offset;
        previous_width = next_width;
    }
    editor.text().len()
}

/// Paints the address context menu over the already composed chrome and page.
pub fn paint_address_context_menu(
    canvas: &mut Canvas<'_>,
    menu: &AddressContextMenu,
    theme: ChromeTheme,
    pointer: Point,
    text: &impl TextPainter,
) {
    let palette = Palette::new(theme);
    let scale = menu.scale;
    canvas.rounded_rect(
        Rect {
            x: menu.bounds.x + 2.0 * scale,
            y: menu.bounds.y + 3.0 * scale,
            ..menu.bounds
        },
        8.0 * scale,
        palette.menu_shadow,
    );
    canvas.rounded_rect(menu.bounds, 8.0 * scale, palette.menu_border);
    canvas.rounded_rect(
        menu.bounds.inset(1.0 * scale),
        7.0 * scale,
        palette.menu_background,
    );
    let font_size = 13.0 * scale;
    for item in &menu.items {
        if item.enabled && item.bounds.contains(pointer) {
            canvas.rounded_rect(item.bounds, 4.0 * scale, palette.menu_hover);
        }
        let color = if item.enabled {
            palette.text
        } else {
            palette.menu_disabled
        };
        let label = address_command_label(item.command);
        text.paint(
            canvas,
            label,
            Point {
                x: item.bounds.x + 10.0 * scale,
                y: item.bounds.y + 7.0 * scale,
            },
            font_size,
            color,
        );
        let shortcut = address_command_shortcut(item.command);
        if !shortcut.is_empty() {
            let shortcut_width = text.measure(shortcut, font_size);
            text.paint(
                canvas,
                shortcut,
                Point {
                    x: item.bounds.x + item.bounds.width - shortcut_width - 10.0 * scale,
                    y: item.bounds.y + 7.0 * scale,
                },
                font_size,
                color,
            );
        }
    }
}

const fn address_command_label(command: AddressCommand) -> &'static str {
    match command {
        AddressCommand::Undo => "Undo",
        AddressCommand::Redo => "Redo",
        AddressCommand::Cut => "Cut",
        AddressCommand::Copy => "Copy",
        AddressCommand::Paste => "Paste",
        AddressCommand::Delete => "Delete",
        AddressCommand::SelectAll => "Select all",
    }
}

const fn address_command_shortcut(command: AddressCommand) -> &'static str {
    match command {
        AddressCommand::Undo => "Ctrl+Z",
        AddressCommand::Redo => "Ctrl+Y",
        AddressCommand::Cut => "Ctrl+X",
        AddressCommand::Copy => "Ctrl+C",
        AddressCommand::Paste => "Ctrl+V",
        AddressCommand::Delete => "",
        AddressCommand::SelectAll => "Ctrl+A",
    }
}

fn paint_close_icon(canvas: &mut Canvas<'_>, rect: Rect, scale: f32, color: u32) {
    let inset = 8.0 * scale;
    canvas.line(
        Point {
            x: rect.x + inset,
            y: rect.y + inset,
        },
        Point {
            x: rect.x + rect.width - inset,
            y: rect.y + rect.height - inset,
        },
        1.25 * scale,
        color,
    );
    canvas.line(
        Point {
            x: rect.x + rect.width - inset,
            y: rect.y + inset,
        },
        Point {
            x: rect.x + inset,
            y: rect.y + rect.height - inset,
        },
        1.25 * scale,
        color,
    );
}

#[allow(
    clippy::too_many_lines,
    reason = "small vector icons are easiest to audit as one exhaustive match"
)]
fn paint_toolbar_icon(canvas: &mut Canvas<'_>, geometry: ButtonGeometry, scale: f32, color: u32) {
    let bounds = geometry.bounds;
    let center = Point {
        x: bounds.x + bounds.width * 0.5,
        y: bounds.y + bounds.height * 0.5,
    };
    let line = 1.7 * scale;
    match geometry.button {
        ToolbarButton::Back | ToolbarButton::Forward => {
            let direction = if geometry.button == ToolbarButton::Back {
                -1.0
            } else {
                1.0
            };
            canvas.line(
                Point {
                    x: center.x - 6.0 * direction * scale,
                    y: center.y,
                },
                Point {
                    x: center.x + 6.0 * direction * scale,
                    y: center.y,
                },
                line,
                color,
            );
            canvas.line(
                Point {
                    x: center.x - 6.0 * direction * scale,
                    y: center.y,
                },
                Point {
                    x: center.x - direction * scale,
                    y: center.y - 5.0 * scale,
                },
                line,
                color,
            );
            canvas.line(
                Point {
                    x: center.x - 6.0 * direction * scale,
                    y: center.y,
                },
                Point {
                    x: center.x - direction * scale,
                    y: center.y + 5.0 * scale,
                },
                line,
                color,
            );
        }
        ToolbarButton::Reload => {
            let radius = 6.5 * scale;
            let start = 0.55_f32;
            let end = std::f32::consts::TAU - 0.55_f32;
            let segments = 24;
            for segment in 0..segments {
                let first = start + (end - start) * segment as f32 / segments as f32;
                let second = start + (end - start) * (segment + 1) as f32 / segments as f32;
                canvas.line(
                    Point {
                        x: first.cos().mul_add(radius, center.x),
                        y: first.sin().mul_add(radius, center.y),
                    },
                    Point {
                        x: second.cos().mul_add(radius, center.x),
                        y: second.sin().mul_add(radius, center.y),
                    },
                    line,
                    color,
                );
            }
            let tangent = Point {
                x: -end.sin(),
                y: end.cos(),
            };
            let wing = Point {
                x: -tangent.y,
                y: tangent.x,
            };
            let arc_end = Point {
                x: end.cos().mul_add(radius, center.x),
                y: end.sin().mul_add(radius, center.y),
            };
            // Arrowhead as a closed triangle straddling the arc end: the tip
            // extends past the arc into the gap so the head reads as an
            // arrow pointing along the arc direction instead of a nub.
            let tip = Point {
                x: tangent.x.mul_add(1.5 * scale, arc_end.x),
                y: tangent.y.mul_add(1.5 * scale, arc_end.y),
            };
            let base = Point {
                x: tangent.x.mul_add(-3.5 * scale, arc_end.x),
                y: tangent.y.mul_add(-3.5 * scale, arc_end.y),
            };
            let wing_a = Point {
                x: wing.x.mul_add(3.0 * scale, base.x),
                y: wing.y.mul_add(3.0 * scale, base.y),
            };
            let wing_b = Point {
                x: wing.x.mul_add(-3.0 * scale, base.x),
                y: wing.y.mul_add(-3.0 * scale, base.y),
            };
            canvas.line(tip, wing_a, line, color);
            canvas.line(tip, wing_b, line, color);
            canvas.line(wing_a, wing_b, line, color);
        }
        ToolbarButton::Home => {
            canvas.line(
                Point {
                    x: center.x - 7.0 * scale,
                    y: center.y,
                },
                Point {
                    x: center.x,
                    y: center.y - 6.0 * scale,
                },
                line,
                color,
            );
            canvas.line(
                Point {
                    x: center.x,
                    y: center.y - 6.0 * scale,
                },
                Point {
                    x: center.x + 7.0 * scale,
                    y: center.y,
                },
                line,
                color,
            );
            canvas.line(
                Point {
                    x: center.x - 5.0 * scale,
                    y: center.y - 1.0 * scale,
                },
                Point {
                    x: center.x - 5.0 * scale,
                    y: center.y + 7.0 * scale,
                },
                line,
                color,
            );
            canvas.line(
                Point {
                    x: center.x - 5.0 * scale,
                    y: center.y + 7.0 * scale,
                },
                Point {
                    x: center.x + 5.0 * scale,
                    y: center.y + 7.0 * scale,
                },
                line,
                color,
            );
            canvas.line(
                Point {
                    x: center.x + 5.0 * scale,
                    y: center.y + 7.0 * scale,
                },
                Point {
                    x: center.x + 5.0 * scale,
                    y: center.y - 1.0 * scale,
                },
                line,
                color,
            );
        }
    }
}

/// Geometry of the page-content scrollbar for one frame. `None` when the
/// content does not overflow the viewport (nothing to scroll, nothing drawn).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ScrollbarGeometry {
    pub track: Rect,
    pub thumb: Rect,
}

/// Scrollbar metrics: an overlay strip on the content's right edge whose
/// thumb length reflects the viewport/content ratio and whose position maps
/// the scroll offset through the remaining travel.
#[must_use]
pub fn scrollbar_geometry(
    content: Rect,
    content_height: f32,
    viewport_height: f32,
    scroll_y: f32,
    scale: f32,
) -> Option<ScrollbarGeometry> {
    let max_scroll = content_height - viewport_height;
    if !(max_scroll > 0.0 && content.height > 0.0) {
        return None;
    }
    let width = 11.0 * scale;
    let margin = 2.0 * scale;
    let track = Rect {
        x: content.x + content.width - width,
        y: content.y,
        width,
        height: content.height,
    };
    let travel = track.height - margin * 2.0;
    let thumb_height =
        (viewport_height / content_height * travel).clamp(24.0 * scale, travel.max(0.0));
    let scroll_ratio = (scroll_y / max_scroll).clamp(0.0, 1.0);
    let thumb_y = track.y + margin + scroll_ratio * (travel - thumb_height);
    Some(ScrollbarGeometry {
        track,
        thumb: Rect {
            x: track.x + margin,
            y: thumb_y,
            width: track.width - margin * 2.0,
            height: thumb_height,
        },
    })
}

/// Inverse mapping for thumb dragging: the thumb follows the pointer while
/// preserving the grab offset inside it, and the scroll offset follows the
/// thumb through the track's travel.
#[must_use]
pub fn scrollbar_scroll_offset(
    geometry: &ScrollbarGeometry,
    pointer_y: f32,
    grab_offset: f32,
    content_height: f32,
    viewport_height: f32,
    scale: f32,
) -> f32 {
    let max_scroll = (content_height - viewport_height).max(0.0);
    if max_scroll <= 0.0 {
        return 0.0;
    }
    let margin = 2.0 * scale;
    let travel = geometry.track.height - margin * 2.0 - geometry.thumb.height;
    // A thumb as tall as the track has no travel to map, and a degenerate
    // geometry must not yield a NaN offset.
    if travel.is_nan() || travel <= 0.0 {
        return 0.0;
    }
    let thumb_y = pointer_y - grab_offset;
    let ratio = ((thumb_y - (geometry.track.y + margin)) / travel).clamp(0.0, 1.0);
    ratio * max_scroll
}

fn elide(text_value: &str, max_width: f32, size: f32, text: &impl TextPainter) -> String {
    if text.measure(text_value, size) <= max_width {
        return text_value.to_owned();
    }
    let mut result = String::new();
    for character in text_value.chars() {
        result.push(character);
        if text.measure(&format!("{result}…"), size) > max_width {
            result.pop();
            break;
        }
    }
    result.push('…');
    result
}

#[derive(Clone, Copy)]
struct Palette {
    tab_strip: u32,
    toolbar: u32,
    tab_hover: u32,
    button_hover: u32,
    address: u32,
    address_focused: u32,
    text: u32,
    icon: u32,
    accent: u32,
    selection: u32,
    close_hover: u32,
    menu_background: u32,
    menu_border: u32,
    menu_hover: u32,
    menu_disabled: u32,
    menu_shadow: u32,
}

impl Palette {
    const fn new(theme: ChromeTheme) -> Self {
        match theme {
            ChromeTheme::Light => Self {
                tab_strip: 0x00e8_eaf0,
                toolbar: 0x00f8_f9fb,
                tab_hover: 0x00d9_dde5,
                button_hover: 0x00e6_e9ef,
                address: 0x00ea_ecf1,
                address_focused: 0x00ff_ffff,
                text: 0x0020_2430,
                icon: 0x004c_5361,
                accent: 0x001a_73e8,
                selection: 0x00c9_defa,
                close_hover: 0x00e8_1148,
                menu_background: 0x00ff_ffff,
                menu_border: 0x00c9_cdd5,
                menu_hover: 0x00e8_eef8,
                menu_disabled: 0x0093_98a3,
                menu_shadow: 0x006e_727a,
            },
            ChromeTheme::Dark => Self {
                tab_strip: 0x001e_2026,
                toolbar: 0x002b_2e35,
                tab_hover: 0x0037_3b44,
                button_hover: 0x003c_4049,
                address: 0x001f_2228,
                address_focused: 0x0018_1a1f,
                text: 0x00ed_eff4,
                icon: 0x00c6_cad3,
                accent: 0x006e_a8fe,
                selection: 0x0033_5f95,
                close_hover: 0x00c4_2b1c,
                menu_background: 0x002b_2e35,
                menu_border: 0x004b_505b,
                menu_hover: 0x003c_4657,
                menu_disabled: 0x007d_828c,
                menu_shadow: 0x000c_0d0f,
            },
        }
    }
}

/// Accent used for focus indicators in both the browser chrome and focused
/// page controls. Keeping this in one place prevents the page caret from
/// becoming unreadable when the native theme changes.
#[must_use]
pub const fn focus_accent(theme: ChromeTheme) -> u32 {
    match theme {
        ChromeTheme::Light => 0x001a_73e8,
        ChromeTheme::Dark => 0x006e_a8fe,
    }
}

fn blend(background: u32, foreground: u32, alpha: u8) -> u32 {
    let alpha = u32::from(alpha);
    let inverse = 255 - alpha;
    let channel = |shift: u32| {
        let back = (background >> shift) & 0xff_u32;
        let front = (foreground >> shift) & 0xff_u32;
        (front * alpha + back * inverse + 127) / 255
    };
    (channel(16) << 16) | (channel(8) << 8) | channel(0)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{
        AddressClickTracker, AddressContextMenu, ButtonGeometry, Canvas, ChromeLayout, HitTarget,
        Point, Rect, TabDrag, TextPainter, TitleBarClickTracker, TitleBarGesture, ToolbarButton,
        WindowAction, WindowControl, address_index_at_x, clamp_interval, paint_toolbar_icon,
    };
    use crate::editor::{AddressCommand, AddressEditor};
    use crate::model::{TabIntent, TabModel};

    struct FixedText;

    impl TextPainter for FixedText {
        fn measure(&self, text: &str, size: f32) -> f32 {
            text.chars().count() as f32 * size
        }

        fn paint(
            &self,
            _canvas: &mut Canvas<'_>,
            _text: &str,
            _origin: Point,
            _size: f32,
            _color: u32,
        ) {
        }
    }

    #[test]
    fn canvas_clip_limits_paint_to_the_requested_rectangle() {
        let mut pixels = [0_u32; 4];
        let mut canvas = Canvas::new(&mut pixels, 4, 1);
        canvas.with_clip(
            Rect {
                x: 1.0,
                y: 0.0,
                width: 2.0,
                height: 1.0,
            },
            |canvas| {
                canvas.rect(
                    Rect {
                        x: 0.0,
                        y: 0.0,
                        width: 4.0,
                        height: 1.0,
                    },
                    7,
                );
            },
        );

        assert_eq!(pixels, [0, 7, 7, 0]);
    }

    #[test]
    fn reload_icon_draws_arrowhead_beyond_the_arc_end() {
        const SIZE: u32 = 32;
        const BACKGROUND: u32 = 0xFF_FAFAFA;
        const ICON: u32 = 0xFF_202124;
        let mut pixels = [BACKGROUND; (SIZE * SIZE) as usize];
        let mut canvas = Canvas::new(&mut pixels, SIZE, SIZE);
        paint_toolbar_icon(
            &mut canvas,
            ButtonGeometry {
                button: ToolbarButton::Reload,
                bounds: Rect {
                    x: 0.0,
                    y: 0.0,
                    width: SIZE as f32,
                    height: SIZE as f32,
                },
            },
            1.0,
            ICON,
        );
        let colored = |x: u32, y: u32| pixels[(y * SIZE + x) as usize] != BACKGROUND;
        // The arrowhead tip reaches past the arc end into the arc gap.
        assert!(colored(22, 13), "missing arrowhead tip beyond the arc");
        // Both wings of the head are drawn.
        assert!(colored(17, 11), "missing inward arrowhead wing");
        assert!(colored(22, 8), "missing outward arrowhead wing");
    }

    #[test]
    fn hit_testing_distinguishes_tab_close_and_address() {
        let tabs = TabModel::new("One", "about:home");
        let layout = ChromeLayout::new(1_000, 700, 1.0, tabs.tabs());
        let tab = layout.tabs[0];
        assert_eq!(
            layout.hit_test(Point {
                x: tab.close.x + 2.0,
                y: tab.close.y + 2.0,
            }),
            HitTarget::CloseTab(tab.id)
        );
        assert_eq!(
            layout.hit_test(Point {
                x: layout.address.x + 10.0,
                y: layout.address.y + 10.0,
            }),
            HitTarget::AddressBar
        );
    }

    #[test]
    fn title_strip_distinguishes_tabs_drag_area_and_window_controls() {
        let tabs = TabModel::new("One", "about:home");
        let layout = ChromeLayout::new(1_000, 700, 1.0, tabs.tabs());
        let tab = layout.tabs[0];
        assert_eq!(
            layout.hit_test(Point {
                x: tab.bounds.x + 0.1,
                y: tab.bounds.y + 0.1,
            }),
            HitTarget::Chrome
        );
        assert_eq!(
            layout.hit_test(Point {
                x: tab.bounds.x + 0.1,
                y: tab.bounds.y + tab.bounds.height - 0.1,
            }),
            HitTarget::Tab(tab.id)
        );
        assert_eq!(
            layout.hit_test(Point {
                x: layout.title_bar.x + layout.title_bar.width * 0.5,
                y: layout.title_bar.height * 0.5,
            }),
            HitTarget::TitleBar
        );
        for geometry in &layout.window_controls {
            assert_eq!(
                layout.hit_test(Point {
                    x: geometry.bounds.x + geometry.bounds.width * 0.5,
                    y: geometry.bounds.y + geometry.bounds.height * 0.5,
                }),
                HitTarget::WindowControl(geometry.control)
            );
        }
    }

    #[test]
    fn window_controls_map_to_platform_actions() {
        assert_eq!(WindowControl::Minimize.action(), WindowAction::Minimize);
        assert_eq!(
            WindowControl::MaximizeRestore.action(),
            WindowAction::ToggleMaximize
        );
        assert_eq!(WindowControl::Close.action(), WindowAction::Close);
    }

    #[test]
    fn title_bar_double_click_toggles_maximize() {
        let mut clicks = TitleBarClickTracker::default();
        let point = Point { x: 500.0, y: 20.0 };
        assert_eq!(
            clicks.register(Duration::from_secs(1), point, 1.0),
            TitleBarGesture::BeginDrag
        );
        assert_eq!(
            clicks.register(Duration::from_millis(1_350), point, 1.0),
            TitleBarGesture::ToggleMaximize
        );
        assert_eq!(
            clicks.register(Duration::from_secs(2), point, 1.0),
            TitleBarGesture::BeginDrag
        );
        assert_eq!(
            clicks.register(
                Duration::from_millis(2_200),
                Point { x: 520.0, y: 20.0 },
                1.0,
            ),
            TitleBarGesture::BeginDrag
        );
    }

    #[test]
    fn tab_shape_rounds_only_the_top_corners() {
        let mut pixels = vec![0; 16 * 16];
        let mut canvas = Canvas::new(&mut pixels, 16, 16);
        canvas.top_rounded_rect(
            Rect {
                x: 2.0,
                y: 2.0,
                width: 10.0,
                height: 10.0,
            },
            4.0,
            0x00ff_ffff,
        );

        assert_eq!(pixels[2 * 16 + 2], 0);
        assert_eq!(pixels[11 * 16 + 2], 0x00ff_ffff);
        assert_eq!(pixels[11 * 16 + 11], 0x00ff_ffff);
    }

    #[test]
    fn drag_commits_reorder_only_on_release() {
        let mut tabs = TabModel::new("One", "about:home");
        let first = tabs.active_id();
        tabs.apply(TabIntent::New);
        let layout = ChromeLayout::new(1_000, 700, 1.0, tabs.tabs());
        let start = layout.tabs[0].bounds.x + 10.0;
        let mut drag = TabDrag::new(first, start);
        // Below the activation threshold nothing moves or commits.
        assert_eq!(drag.paint_offsets(start + 2.0, &layout), None);
        assert_eq!(drag.release(start + 2.0, &layout), None);
        // Activating only changes paint geometry; the model order is untouched
        // until release.
        let crossing = layout.tabs[1].bounds.x + layout.tabs[1].bounds.width;
        assert!(drag.paint_offsets(crossing, &layout).is_some());
        assert_eq!(
            drag.release(crossing, &layout),
            Some(TabIntent::Move {
                tab: first,
                index: 1,
            })
        );
    }

    #[test]
    fn live_drag_moves_dragged_tab_continuously_and_slides_neighbors() {
        let mut tabs = TabModel::new("One", "about:home");
        let first = tabs.active_id();
        tabs.apply(TabIntent::New);
        tabs.apply(TabIntent::New);
        let layout = ChromeLayout::new(1_000, 700, 1.0, tabs.tabs());
        let start = layout.tabs[0].bounds.x + 10.0;
        let mut drag = TabDrag::new(first, start);
        let static_second = layout.tabs[1].bounds.x;
        let stride = static_second - layout.tabs[0].bounds.x;

        // Midway into the first gap the dragged tab has moved continuously and
        // the neighbor is partially displaced.
        let midway = start + stride * 0.5;
        let offsets = drag.paint_offsets(midway, &layout).expect("active drag");
        let dragged_static = layout.tabs[0].bounds.x;
        assert!(
            (offsets[0] - stride * 0.5).abs() < 0.5,
            "dragged tab follows the pointer, got {}",
            offsets[0]
        );
        assert!(
            offsets[1] < 0.0 && offsets[1] > -stride,
            "neighbor slides left without overshooting, got {}",
            offsets[1]
        );
        assert!(
            offsets[2].abs() < 0.001,
            "far tabs do not move yet, got {}",
            offsets[2]
        );

        // Far right: the dragged tab clamps to the last slot; the two tabs in
        // between sit exactly one slot left; nothing else moves.
        let far = layout.tabs[0].bounds.x + layout.tabs[2].bounds.x + 1_000.0;
        let offsets = drag.paint_offsets(far, &layout).expect("active drag");
        let dragged_final = dragged_static + offsets[0];
        assert!(
            (dragged_final - layout.tabs[2].bounds.x).abs() < 0.5,
            "dragged tab rests on the last slot, got {dragged_final}"
        );
        assert!(
            (offsets[1] + stride).abs() < 0.5,
            "first neighbor slid one slot left"
        );
        assert!(
            (offsets[2] + stride).abs() < 0.5,
            "second neighbor slid one slot left"
        );
        assert_eq!(
            drag.release(far, &layout),
            Some(TabIntent::Move {
                tab: first,
                index: 2,
            })
        );

        // Far left of the origin: neighbors slide right by one slot instead.
        let mut drag = TabDrag::new(tabs.tabs()[2].id, layout.tabs[2].bounds.x + 10.0);
        let far_left = layout.tabs[0].bounds.x - 1_000.0;
        let offsets = drag.paint_offsets(far_left, &layout).expect("active drag");
        assert!(
            (offsets[0] - stride).abs() < 0.5,
            "left neighbor slid one slot right"
        );
        assert!(
            (offsets[1] - stride).abs() < 0.5,
            "left neighbor slid one slot right"
        );
        assert!(
            (offsets[2] + 2.0 * stride).abs() < 0.5,
            "dragged tab rests on the first slot, got {}",
            offsets[2]
        );
    }

    #[test]
    fn chrome_height_scales_with_system_dpi() {
        let tabs = TabModel::new("One", "about:home");
        let normal = ChromeLayout::new(800, 600, 1.0, tabs.tabs());
        let high_dpi = ChromeLayout::new(1_600, 1_200, 2.0, tabs.tabs());
        assert_eq!(high_dpi.chrome_height, normal.chrome_height * 2);
    }

    #[test]
    fn address_hit_testing_uses_nearest_ascii_and_cjk_boundaries_at_each_dpi() {
        let tabs = TabModel::new("One", "about:home");
        let text = FixedText;
        let editor = AddressEditor::new("ab中");
        for (scale, width, height) in [(1.0, 800, 600), (1.5, 1_200, 900), (2.0, 1_600, 1_200)] {
            let layout = ChromeLayout::new(width, height, scale, tabs.tabs());
            let origin = layout.address.x + 16.0 * scale;
            let advance = 14.0 * scale;
            assert_eq!(
                address_index_at_x(&layout, &editor, origin - 20.0, &text),
                0
            );
            assert_eq!(
                address_index_at_x(&layout, &editor, origin + advance * 0.25, &text),
                0
            );
            assert_eq!(
                address_index_at_x(&layout, &editor, origin + advance * 0.75, &text),
                1
            );
            assert_eq!(
                address_index_at_x(&layout, &editor, origin + advance * 1.75, &text),
                2
            );
            assert_eq!(
                address_index_at_x(&layout, &editor, origin + advance * 2.75, &text),
                5
            );
            assert_eq!(
                address_index_at_x(&layout, &editor, origin + advance * 20.0, &text),
                5
            );
        }
    }

    #[test]
    fn address_hit_testing_tracks_horizontal_text_scroll() {
        let tabs = TabModel::new("One", "about:home");
        let layout = ChromeLayout::new(560, 360, 1.0, tabs.tabs());
        let text = FixedText;
        let editor = AddressEditor::new("abcdefghijklmnopqrstuvwxyz0123456789");
        let visible_caret_x = layout.address.x + layout.address.width - 24.0;
        assert_eq!(
            address_index_at_x(&layout, &editor, visible_caret_x + 50.0, &text),
            editor.text().len()
        );
    }

    #[test]
    fn address_double_click_tracker_respects_time_distance_and_dpi() {
        let mut clicks = AddressClickTracker::default();
        let point = Point { x: 300.0, y: 70.0 };
        assert!(!clicks.register(Duration::from_secs(1), point, 2.0));
        assert!(clicks.register(
            Duration::from_millis(1_300),
            Point { x: 307.0, y: 70.0 },
            2.0
        ));
        assert!(!clicks.register(Duration::from_secs(2), point, 1.0));
        assert!(!clicks.register(
            Duration::from_millis(2_200),
            Point { x: 310.0, y: 70.0 },
            1.0
        ));
    }

    #[test]
    fn address_context_menu_clamps_scales_and_reports_disabled_items() {
        let editor = AddressEditor::new("value");
        let menu =
            AddressContextMenu::new(Point { x: 790.0, y: 590.0 }, 800, 600, 1.5, &editor, false);
        assert!(menu.bounds.x >= 0.0);
        assert!(menu.bounds.y >= 0.0);
        assert!(menu.bounds.x + menu.bounds.width <= 800.0);
        assert!(menu.bounds.y + menu.bounds.height <= 600.0);
        assert_eq!(menu.items.len(), 6);
        let paste = menu
            .items
            .iter()
            .find(|item| item.command == AddressCommand::Paste)
            .expect("paste item");
        assert!(!paste.enabled);
        assert_eq!(
            menu.item_at(Point {
                x: paste.bounds.x + 1.0,
                y: paste.bounds.y + 1.0,
            }),
            Some(*paste)
        );
    }

    #[test]
    fn crowded_tab_close_targets_stay_inside_their_tabs() {
        let mut tabs = TabModel::new("One", "about:home");
        for _ in 0..31 {
            tabs.apply(TabIntent::New);
        }
        let layout = ChromeLayout::new(560, 360, 1.0, tabs.tabs());
        for tab in &layout.tabs {
            assert!(tab.close.x >= tab.bounds.x);
            assert!(tab.close.x + tab.close.width <= tab.bounds.x + tab.bounds.width);
        }
    }

    #[test]
    fn address_rounding_is_stable_at_fractional_dpi() {
        let tabs = TabModel::new("One", "about:home");
        for (scale, width, height) in [(1.25, 1_000, 750), (1.5, 1_200, 900)] {
            let layout = ChromeLayout::new(width, height, scale, tabs.tabs());
            let mut pixels = vec![0; width as usize * height as usize];
            let mut canvas = Canvas::new(&mut pixels, width, height);

            canvas.rounded_rect(layout.address, layout.address.height * 0.5, 0x00ff_ffff);

            assert!(pixels.contains(&0x00ff_ffff));
        }
    }

    #[test]
    fn subpixel_rounding_can_collapse_an_inverted_interval() {
        let result = clamp_interval(54.0, 54.0, 53.999_996);
        assert!(result.is_finite());
        assert!((53.999_996..=54.0).contains(&result));
    }
}

#[cfg(test)]
mod scrollbar_tests {
    use super::{Point, Rect, ScrollbarGeometry, scrollbar_geometry, scrollbar_scroll_offset};

    const CONTENT: Rect = Rect {
        x: 0.0,
        y: 100.0,
        width: 1000.0,
        height: 500.0,
    };

    #[test]
    fn no_geometry_without_overflow() {
        assert!(scrollbar_geometry(CONTENT, 500.0, 500.0, 0.0, 1.0).is_none());
        assert!(scrollbar_geometry(CONTENT, 400.0, 500.0, 0.0, 1.0).is_none());
    }

    #[test]
    fn thumb_length_reflects_viewport_ratio_and_position_maps_offset() {
        let geometry = scrollbar_geometry(CONTENT, 2000.0, 500.0, 0.0, 1.0).expect("scrollbar");
        // Quarter of the content is visible; thumb is a quarter of travel,
        // clamped to at least 24px.
        // travel = 500 - 2*2(margin) = 496; ratio 500/2000 -> 124px
        assert!((geometry.thumb.height - 124.0).abs() < 0.01);
        assert!((geometry.thumb.y - (geometry.track.y + 2.0)).abs() < 0.01);
        assert!((geometry.thumb.x + geometry.thumb.width - (CONTENT.width - 2.0)).abs() < 0.01);

        let bottom = scrollbar_geometry(CONTENT, 2000.0, 500.0, 1500.0, 1.0).expect("scrollbar");
        assert!(
            (bottom.thumb.y + bottom.thumb.height
                - (geometry.track.y + geometry.track.height - 2.0))
                .abs()
                < 0.01
        );
    }

    #[test]
    fn dragging_maps_pointer_back_to_scroll_offset() {
        let geometry = scrollbar_geometry(CONTENT, 2000.0, 500.0, 0.0, 1.0).expect("scrollbar");
        // Grab the thumb at its top (grab offset 0) and drag to the bottom
        // of the travel: the offset must reach max_scroll.
        let travel_bottom = geometry.track.y + geometry.track.height - 2.0 - geometry.thumb.height;
        let bottom = scrollbar_scroll_offset(&geometry, travel_bottom, 0.0, 2000.0, 500.0, 1.0);
        assert!((bottom - 1500.0).abs() < 0.01, "{bottom}");
        let top =
            scrollbar_scroll_offset(&geometry, geometry.track.y + 2.0, 0.0, 2000.0, 500.0, 1.0);
        assert!((top - 0.0).abs() < 0.01, "{top}");
    }

    #[test]
    fn track_hit_and_thumb_grab_use_the_same_geometry() {
        let geometry = scrollbar_geometry(CONTENT, 2000.0, 500.0, 375.0, 1.0).expect("scrollbar");
        // A pointer inside the thumb is inside the track too (single hit
        // region), and the thumb's y is between the track bounds.
        let center = Point {
            x: geometry.thumb.x + 1.0,
            y: geometry.thumb.y + 1.0,
        };
        assert!(geometry.track.contains(center));
    }

    #[test]
    fn thumb_minimum_length_keeps_tiny_viewport_ratios_grabbable() {
        let geometry = scrollbar_geometry(CONTENT, 100_000.0, 500.0, 0.0, 1.0).expect("scrollbar");
        assert!((geometry.thumb.height - 24.0).abs() < 0.01);
        let _ = ScrollbarGeometry {
            track: geometry.track,
            thumb: geometry.thumb,
        };
    }
}
