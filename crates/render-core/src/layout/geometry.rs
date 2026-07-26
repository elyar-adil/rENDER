//! Logical and physical CSS geometry primitives.

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct LogicalSize {
    pub inline: f32,
    pub block: f32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct LogicalPoint {
    pub inline: f32,
    pub block: f32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct LogicalRect {
    pub origin: LogicalPoint,
    pub size: LogicalSize,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct PhysicalSize {
    pub width: f32,
    pub height: f32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct PhysicalPoint {
    pub x: f32,
    pub y: f32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct PhysicalRect {
    pub origin: PhysicalPoint,
    pub size: PhysicalSize,
}

impl PhysicalRect {
    #[must_use]
    pub const fn new(x: f32, y: f32, width: f32, height: f32) -> Self {
        Self {
            origin: PhysicalPoint { x, y },
            size: PhysicalSize { width, height },
        }
    }

    #[must_use]
    pub fn right(self) -> f32 {
        self.origin.x + self.size.width
    }

    #[must_use]
    pub fn bottom(self) -> f32 {
        self.origin.y + self.size.height
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct EdgeSizes {
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
    pub left: f32,
}

impl EdgeSizes {
    #[must_use]
    pub fn horizontal(self) -> f32 {
        self.left + self.right
    }

    #[must_use]
    pub fn vertical(self) -> f32 {
        self.top + self.bottom
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum WritingMode {
    #[default]
    HorizontalTb,
    VerticalRl,
    VerticalLr,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Direction {
    #[default]
    Ltr,
    Rtl,
}

impl LogicalRect {
    /// Convert logical coordinates within a physical containing block. Keeping
    /// this conversion at fragment boundaries lets formatting algorithms stay
    /// axis-agnostic.
    #[must_use]
    pub fn to_physical(
        self,
        containing: PhysicalRect,
        writing_mode: WritingMode,
        direction: Direction,
    ) -> PhysicalRect {
        match writing_mode {
            WritingMode::HorizontalTb => {
                let x = match direction {
                    Direction::Ltr => containing.origin.x + self.origin.inline,
                    Direction::Rtl => containing.right() - self.origin.inline - self.size.inline,
                };
                PhysicalRect::new(
                    x,
                    containing.origin.y + self.origin.block,
                    self.size.inline,
                    self.size.block,
                )
            }
            WritingMode::VerticalRl => PhysicalRect::new(
                containing.right() - self.origin.block - self.size.block,
                containing.origin.y + self.origin.inline,
                self.size.block,
                self.size.inline,
            ),
            WritingMode::VerticalLr => PhysicalRect::new(
                containing.origin.x + self.origin.block,
                containing.origin.y + self.origin.inline,
                self.size.block,
                self.size.inline,
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Direction, LogicalPoint, LogicalRect, LogicalSize, PhysicalRect, WritingMode};

    #[test]
    fn logical_geometry_supports_horizontal_rtl_and_vertical_flows() {
        let logical = LogicalRect {
            origin: LogicalPoint {
                inline: 10.0,
                block: 20.0,
            },
            size: LogicalSize {
                inline: 30.0,
                block: 40.0,
            },
        };
        let containing = PhysicalRect::new(100.0, 200.0, 300.0, 400.0);
        assert_eq!(
            logical.to_physical(containing, WritingMode::HorizontalTb, Direction::Rtl),
            PhysicalRect::new(360.0, 220.0, 30.0, 40.0)
        );
        assert_eq!(
            logical.to_physical(containing, WritingMode::VerticalRl, Direction::Ltr),
            PhysicalRect::new(340.0, 210.0, 40.0, 30.0)
        );
    }
}
