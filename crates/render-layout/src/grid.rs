//! Bounded, deterministic helpers for the first Grid layout slice.
//!
//! DOM/style access and fragment construction stay in the reference solver;
//! this module owns track repetition, row-major placement and axis sizing so
//! those algorithms can be tested independently.

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum TrackSizing {
    Fixed(f32),
    Flexible { minimum: f32, factor: f32 },
    Intrinsic { minimum: f32 },
}

impl TrackSizing {
    pub(crate) fn minimum(self) -> f32 {
        match self {
            Self::Fixed(value) => value,
            Self::Flexible { minimum, .. } | Self::Intrinsic { minimum } => minimum,
        }
        .max(0.0)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SizedAxis {
    sizes: Vec<f32>,
    offsets: Vec<f32>,
    extent: f32,
}

impl SizedAxis {
    pub(crate) fn size(&self, index: usize) -> f32 {
        self.sizes.get(index).copied().unwrap_or(0.0)
    }

    pub(crate) fn offset(&self, index: usize) -> f32 {
        self.offsets.get(index).copied().unwrap_or(0.0)
    }

    pub(crate) const fn extent(&self) -> f32 {
        self.extent
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum GridLimitError {
    TrackLimit,
}

/// Expand a standalone auto-repeat once the available size is definite.
/// `auto-fit` collapses repetitions that have no item; `auto-fill` retains all
/// repetitions that fit. The expansion is checked before allocating.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
pub(crate) fn expand_auto_repeat(
    pattern: &[TrackSizing],
    available: f32,
    gap: f32,
    item_count: usize,
    auto_fit: bool,
    max_tracks: usize,
) -> Result<Vec<TrackSizing>, GridLimitError> {
    if pattern.is_empty() {
        return Ok(vec![TrackSizing::Flexible {
            minimum: 0.0,
            factor: 1.0,
        }]);
    }
    let pattern_minimum = pattern.iter().map(|track| track.minimum()).sum::<f32>();
    let pattern_gaps = gap.max(0.0) * count_as_f32(pattern.len().saturating_sub(1));
    let denominator = pattern_minimum + pattern_gaps + gap.max(0.0);
    let fitting_repetitions = if denominator > 0.0 {
        (((available.max(0.0) + gap.max(0.0)) / denominator).floor() as usize).max(1)
    } else {
        1
    };
    let repetitions = if auto_fit {
        let occupied = item_count.div_ceil(pattern.len()).max(1);
        fitting_repetitions.min(occupied)
    } else {
        fitting_repetitions
    };
    let track_count = repetitions
        .checked_mul(pattern.len())
        .filter(|count| *count <= max_tracks)
        .ok_or(GridLimitError::TrackLimit)?;
    let mut expanded = Vec::with_capacity(track_count);
    for _ in 0..repetitions {
        expanded.extend_from_slice(pattern);
    }
    Ok(expanded)
}

pub(crate) fn required_rows(item_count: usize, column_count: usize) -> usize {
    item_count.div_ceil(column_count.max(1))
}

pub(crate) fn automatic_position(index: usize, column_count: usize) -> (usize, usize) {
    let columns = column_count.max(1);
    (index / columns, index % columns)
}

/// Size tracks against either a definite available size or intrinsic item
/// contributions. Fixed tracks do not grow for content; flexible and implicit
/// tracks do. Remaining definite space is divided by positive flex factors.
pub(crate) fn size_axis(
    tracks: &[TrackSizing],
    available: Option<f32>,
    gap: f32,
    contributions: &[f32],
) -> SizedAxis {
    let gap = gap.max(0.0);
    let gaps = gap * count_as_f32(tracks.len().saturating_sub(1));
    let mut sizes = tracks
        .iter()
        .enumerate()
        .map(|(index, track)| {
            let contribution = contributions.get(index).copied().unwrap_or(0.0).max(0.0);
            match *track {
                TrackSizing::Fixed(value) => value.max(0.0),
                TrackSizing::Flexible { minimum, .. } | TrackSizing::Intrinsic { minimum } => {
                    minimum.max(contribution).max(0.0)
                }
            }
        })
        .collect::<Vec<_>>();

    if let Some(available) = available {
        let base = sizes.iter().sum::<f32>() + gaps;
        let free = (available.max(0.0) - base).max(0.0);
        let factor_sum = tracks
            .iter()
            .map(|track| match track {
                TrackSizing::Flexible { factor, .. } => factor.max(0.0),
                TrackSizing::Fixed(_) | TrackSizing::Intrinsic { .. } => 0.0,
            })
            .sum::<f32>();
        if free > 0.0 && factor_sum > 0.0 {
            for (size, track) in sizes.iter_mut().zip(tracks) {
                if let TrackSizing::Flexible { factor, .. } = track {
                    *size += free * factor.max(0.0) / factor_sum;
                }
            }
        }
    }

    let mut cursor = 0.0;
    let offsets = sizes
        .iter()
        .map(|size| {
            let offset = cursor;
            cursor += *size + gap;
            offset
        })
        .collect::<Vec<_>>();
    let extent = if sizes.is_empty() { 0.0 } else { cursor - gap };
    SizedAxis {
        sizes,
        offsets,
        extent,
    }
}

#[allow(clippy::cast_precision_loss)]
fn count_as_f32(count: usize) -> f32 {
    u32::try_from(count).map_or(u32::MAX as f32, |count| count as f32)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::float_cmp)]

    use super::{
        GridLimitError, TrackSizing, automatic_position, expand_auto_repeat, required_rows,
        size_axis,
    };

    #[test]
    fn definite_axis_distributes_remaining_space_after_fixed_tracks_and_gaps() {
        let axis = size_axis(
            &[
                TrackSizing::Fixed(100.0),
                TrackSizing::Flexible {
                    minimum: 0.0,
                    factor: 1.0,
                },
                TrackSizing::Flexible {
                    minimum: 0.0,
                    factor: 2.0,
                },
            ],
            Some(430.0),
            15.0,
            &[],
        );
        assert_eq!(axis.size(0), 100.0);
        assert_eq!(axis.size(1), 100.0);
        assert_eq!(axis.size(2), 200.0);
        assert_eq!(axis.offset(2), 230.0);
        assert_eq!(axis.extent(), 430.0);
    }

    #[test]
    fn auto_repeat_is_responsive_bounded_and_auto_fit_uses_occupied_tracks() {
        let pattern = [TrackSizing::Flexible {
            minimum: 140.0,
            factor: 1.0,
        }];
        assert_eq!(
            expand_auto_repeat(&pattern, 620.0, 10.0, 3, true, 32)
                .unwrap()
                .len(),
            3
        );
        assert_eq!(
            expand_auto_repeat(&pattern, 620.0, 10.0, 3, false, 32)
                .unwrap()
                .len(),
            4
        );
        assert_eq!(
            expand_auto_repeat(&pattern, 100_000.0, 0.0, 1_000, false, 8),
            Err(GridLimitError::TrackLimit)
        );
    }

    #[test]
    fn automatic_placement_is_bounded_row_major_arithmetic() {
        assert_eq!(required_rows(7, 3), 3);
        assert_eq!(automatic_position(0, 3), (0, 0));
        assert_eq!(automatic_position(4, 3), (1, 1));
        assert_eq!(automatic_position(6, 3), (2, 0));
    }
}
