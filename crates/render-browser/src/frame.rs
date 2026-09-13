//! Frame composition primitives used by the native browser shell.
//!
//! This module owns damage tracking and pixel-buffer transforms. Keeping these
//! operations separate from application orchestration makes partial redraws
//! easier to reason about and keeps layout/network state out of frame code.

use std::num::NonZeroU32;

use render_core::js::ElementRect;
use render_core::layout::{FragmentKind, FragmentTree};
use render_core::paint::Surface;
use softbuffer::Rect as SoftBufferRect;
use winit::dpi::PhysicalSize;

const MAX_DAMAGE_RECTS: usize = 16;
const DAMAGE_FULL_THRESHOLD_PERCENT: u64 = 75;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct FrameRect {
    pub(crate) x: u32,
    pub(crate) y: u32,
    pub(crate) width: u32,
    pub(crate) height: u32,
}

impl FrameRect {
    fn right(self) -> u32 {
        self.x.saturating_add(self.width)
    }

    fn bottom(self) -> u32 {
        self.y.saturating_add(self.height)
    }

    fn area(self) -> u64 {
        u64::from(self.width) * u64::from(self.height)
    }

    fn touches_or_overlaps(self, other: Self) -> bool {
        self.x <= other.right()
            && other.x <= self.right()
            && self.y <= other.bottom()
            && other.y <= self.bottom()
    }

    fn union(self, other: Self) -> Self {
        let right = self.right().max(other.right());
        let bottom = self.bottom().max(other.bottom());
        let x = self.x.min(other.x);
        let y = self.y.min(other.y);
        Self {
            x,
            y,
            width: right.saturating_sub(x),
            height: bottom.saturating_sub(y),
        }
    }

    fn clip(self, width: u32, height: u32) -> Option<Self> {
        let right = self.right().min(width);
        let bottom = self.bottom().min(height);
        (self.x < right && self.y < bottom).then_some(Self {
            x: self.x,
            y: self.y,
            width: right.saturating_sub(self.x),
            height: bottom.saturating_sub(self.y),
        })
    }
}

#[derive(Clone, Debug, Default)]
pub(crate) struct FrameDamage {
    pub(crate) full: bool,
    pub(crate) rects: Vec<FrameRect>,
}

impl FrameDamage {
    pub(crate) fn mark_full(&mut self) {
        self.full = true;
        self.rects.clear();
    }

    pub(crate) fn mark_rect(&mut self, rect: FrameRect, frame_width: u32, frame_height: u32) {
        if self.full || frame_width == 0 || frame_height == 0 {
            return;
        }
        let Some(mut merged) = rect.clip(frame_width, frame_height) else {
            return;
        };
        let mut index = 0;
        while index < self.rects.len() {
            if self.rects[index].touches_or_overlaps(merged) {
                merged = self.rects[index].union(merged);
                self.rects.swap_remove(index);
            } else {
                index += 1;
            }
        }
        self.rects.push(merged);
        let damaged_area = self.rects.iter().map(|item| item.area()).sum::<u64>();
        let frame_area = u64::from(frame_width) * u64::from(frame_height);
        if self.rects.len() > MAX_DAMAGE_RECTS
            || damaged_area.saturating_mul(100)
                >= frame_area.saturating_mul(DAMAGE_FULL_THRESHOLD_PERCENT)
        {
            self.mark_full();
        }
    }

    pub(crate) fn take_for_present(
        &mut self,
        frame_width: u32,
        frame_height: u32,
    ) -> Vec<SoftBufferRect> {
        if frame_width == 0 || frame_height == 0 {
            self.full = false;
            self.rects.clear();
            return Vec::new();
        }
        if !self.full && self.rects.is_empty() {
            return Vec::new();
        }
        let rects = if self.full || self.rects.is_empty() {
            vec![SoftBufferRect {
                x: 0,
                y: 0,
                width: NonZeroU32::new(frame_width).expect("frame width is non-zero"),
                height: NonZeroU32::new(frame_height).expect("frame height is non-zero"),
            }]
        } else {
            self.rects
                .iter()
                .filter_map(|rect| {
                    Some(SoftBufferRect {
                        x: rect.x,
                        y: rect.y,
                        width: NonZeroU32::new(rect.width)?,
                        height: NonZeroU32::new(rect.height)?,
                    })
                })
                .collect()
        };
        self.full = false;
        self.rects.clear();
        rects
    }
}

pub(crate) fn blit_page(
    destination: &mut [u32],
    destination_size: PhysicalSize<u32>,
    source: &[u32],
    source_size: PhysicalSize<u32>,
    destination_y: u32,
) {
    let copy_width = source_size.width.min(destination_size.width) as usize;
    let copy_height = source_size
        .height
        .min(destination_size.height.saturating_sub(destination_y));
    for row in 0..copy_height {
        let source_start = row as usize * source_size.width as usize;
        let destination_start = (row + destination_y) as usize * destination_size.width as usize;
        destination[destination_start..destination_start + copy_width]
            .copy_from_slice(&source[source_start..source_start + copy_width]);
    }
}

pub(crate) fn copy_frame_regions(
    destination: &mut [u32],
    source: &[u32],
    frame_size: PhysicalSize<u32>,
    regions: &[SoftBufferRect],
) {
    let frame_width = frame_size.width as usize;
    for region in regions {
        let x = region.x as usize;
        let y = region.y as usize;
        let width = region.width.get() as usize;
        let height = region.height.get() as usize;
        for row in 0..height {
            let offset = (y + row) * frame_width + x;
            let end = offset + width;
            destination[offset..end].copy_from_slice(&source[offset..end]);
        }
    }
}

#[allow(
    clippy::cast_precision_loss,
    reason = "native dimensions are bounded far below f32's exact integer range"
)]
pub(crate) fn viewport_dimension(value: u32) -> f32 {
    value as f32
}

pub(crate) fn surface_to_softbuffer(surface: &Surface) -> Vec<u32> {
    surface
        .pixels()
        .iter()
        .map(|color| {
            (u32::from(color.red) << 16) | (u32::from(color.green) << 8) | u32::from(color.blue)
        })
        .collect()
}

pub(crate) fn geometry_from_layout(
    fragments: &FragmentTree,
) -> std::collections::BTreeMap<u64, ElementRect> {
    let mut geometry = std::collections::BTreeMap::new();
    for fragment in fragments.iter() {
        let FragmentKind::Box(box_geometry) = &fragment.kind else {
            continue;
        };
        let Some(source) = fragment.source else {
            continue;
        };
        let rect = box_geometry.border_rect();
        geometry.entry(source.as_u64()).or_insert(ElementRect {
            x: rect.origin.x,
            y: rect.origin.y,
            width: rect.size.width,
            height: rect.size.height,
        });
    }
    geometry
}
