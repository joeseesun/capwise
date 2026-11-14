use serde::{Deserialize, Serialize};
use specta::Type;

use crate::XY;

/// How a value travels from one keyframe to the next. Stored on the keyframe
/// the span leaves.
#[derive(Type, Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[serde(rename_all = "camelCase")]
pub enum KeyframeInterpolation {
    /// Eases out of the first keyframe and into the last, and carries speed
    /// through the ones between without ever passing a keyed value.
    #[default]
    Smooth,
    Linear,
    /// Stays on this keyframe's value until the next one.
    Hold,
}

/// Something a keyframe can hold: a fixed set of numbers that interpolate
/// independently.
pub trait KeyframeValue: Copy {
    type Channels: AsRef<[f64]> + AsMut<[f64]> + Copy;
    fn channels(&self) -> Self::Channels;
    fn from_channels(channels: Self::Channels) -> Self;
}

/// Where a layer is and how it looks at one instant.
#[derive(Type, Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct LayerPose {
    pub center: XY<f64>,
    pub size: XY<f64>,
    #[serde(default)]
    pub rotation: f32,
    #[serde(default = "LayerPose::default_opacity")]
    pub opacity: f32,
}

impl LayerPose {
    fn default_opacity() -> f32 {
        1.0
    }
}

impl KeyframeValue for LayerPose {
    type Channels = [f64; 6];

    fn channels(&self) -> [f64; 6] {
        [
            self.center.x,
            self.center.y,
            self.size.x,
            self.size.y,
            f64::from(self.rotation),
            f64::from(self.opacity),
        ]
    }

    fn from_channels(values: [f64; 6]) -> Self {
        Self {
            center: XY::new(values[0], values[1]),
            size: XY::new(values[2], values[3]),
            rotation: values[4] as f32,
            opacity: values[5] as f32,
        }
    }
}

/// A text layer at one instant. `font_size` is in pixels at the 1080p
/// reference height, like the segment's own.
#[derive(Type, Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TextPose {
    pub center: XY<f64>,
    pub size: XY<f64>,
    pub opacity: f32,
    pub font_size: f32,
}

impl KeyframeValue for TextPose {
    type Channels = [f64; 6];

    fn channels(&self) -> [f64; 6] {
        [
            self.center.x,
            self.center.y,
            self.size.x,
            self.size.y,
            f64::from(self.opacity),
            f64::from(self.font_size),
        ]
    }

    fn from_channels(values: [f64; 6]) -> Self {
        Self {
            center: XY::new(values[0], values[1]),
            size: XY::new(values[2], values[3]),
            opacity: values[4] as f32,
            font_size: values[5] as f32,
        }
    }
}

/// A mask at one instant. `pixelation` is the encoded blur or pixelate
/// amount and `darkness` the highlight's surround, as on the segment.
#[derive(Type, Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct MaskPose {
    pub center: XY<f64>,
    pub size: XY<f64>,
    pub opacity: f64,
    pub pixelation: f64,
    pub darkness: f64,
}

impl KeyframeValue for MaskPose {
    type Channels = [f64; 7];

    fn channels(&self) -> [f64; 7] {
        [
            self.center.x,
            self.center.y,
            self.size.x,
            self.size.y,
            self.opacity,
            self.pixelation,
            self.darkness,
        ]
    }

    fn from_channels(values: [f64; 7]) -> Self {
        Self {
            center: XY::new(values[0], values[1]),
            size: XY::new(values[2], values[3]),
            opacity: values[4],
            pixelation: values[5],
            darkness: values[6],
        }
    }
}

/// A clip's gain at one instant, in decibels.
#[derive(Type, Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct VolumeLevel {
    pub volume_db: f32,
}

impl KeyframeValue for VolumeLevel {
    type Channels = [f64; 1];

    fn channels(&self) -> [f64; 1] {
        [f64::from(self.volume_db)]
    }

    fn from_channels(values: [f64; 1]) -> Self {
        Self {
            volume_db: values[0] as f32,
        }
    }
}

/// One keyframe. `time` is seconds from the owning segment's own start, so
/// moving the segment on the timeline carries its animation with it.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Keyframe<V> {
    pub time: f64,
    #[serde(flatten)]
    pub value: V,
    #[serde(default)]
    pub interpolation: KeyframeInterpolation,
}

pub type LayerKeyframe = Keyframe<LayerPose>;
pub type TextKeyframe = Keyframe<TextPose>;
pub type MaskKeyframe = Keyframe<MaskPose>;
pub type VolumeKeyframe = Keyframe<VolumeLevel>;

fn is_usable<V: KeyframeValue>(key: &Keyframe<V>) -> bool {
    key.time.is_finite() && key.value.channels().as_ref().iter().all(|v| v.is_finite())
}

fn sorted<V: KeyframeValue>(keys: &[Keyframe<V>]) -> std::borrow::Cow<'_, [Keyframe<V>]> {
    if keys.iter().all(is_usable) && keys.is_sorted_by(|a, b| a.time <= b.time) {
        return std::borrow::Cow::Borrowed(keys);
    }
    let mut owned: Vec<_> = keys.iter().copied().filter(is_usable).collect();
    owned.sort_by(|a, b| a.time.total_cmp(&b.time));
    std::borrow::Cow::Owned(owned)
}

fn secant<V: KeyframeValue>(keys: &[Keyframe<V>], from: usize, channel: usize) -> f64 {
    let span = keys[from + 1].time - keys[from].time;
    if span <= 1e-9 {
        return 0.0;
    }
    (keys[from + 1].value.channels().as_ref()[channel]
        - keys[from].value.channels().as_ref()[channel])
        / span
}

/// The slope a smooth span uses at `index`: flat at the ends, at a turning
/// point and next to a hold; otherwise the monotone (Fritsch-Butland) mean of
/// the two neighbouring slopes, which is what keeps the curve from ever
/// passing a keyed value. Next to a linear span it takes that span's slope so
/// the speed does not jump, capped where it would overshoot.
fn tangent<V: KeyframeValue>(keys: &[Keyframe<V>], index: usize, channel: usize) -> f64 {
    if index == 0 || index + 1 >= keys.len() {
        return 0.0;
    }
    let before = keys[index - 1].interpolation;
    let after = keys[index].interpolation;
    if before == KeyframeInterpolation::Hold || after == KeyframeInterpolation::Hold {
        return 0.0;
    }
    let slope_before = secant(keys, index - 1, channel);
    let slope_after = secant(keys, index, channel);
    if slope_before * slope_after <= 0.0 {
        return 0.0;
    }
    match (before, after) {
        (KeyframeInterpolation::Linear, KeyframeInterpolation::Linear) => 0.0,
        (KeyframeInterpolation::Linear, _) => {
            slope_before.signum() * slope_before.abs().min(3.0 * slope_after.abs())
        }
        (_, KeyframeInterpolation::Linear) => {
            slope_after.signum() * slope_after.abs().min(3.0 * slope_before.abs())
        }
        _ => {
            let span_before = keys[index].time - keys[index - 1].time;
            let span_after = keys[index + 1].time - keys[index].time;
            let weight_before = 2.0 * span_after + span_before;
            let weight_after = span_after + 2.0 * span_before;
            (weight_before + weight_after)
                / (weight_before / slope_before + weight_after / slope_after)
        }
    }
}

/// The value at `time` (seconds from the segment start), or `None` when there
/// are no usable keyframes. Holds before the first and after the last.
pub fn sample_keyframes<V: KeyframeValue>(keys: &[Keyframe<V>], time: f64) -> Option<V> {
    let keys = sorted(keys);
    let first = keys.first()?;
    if !time.is_finite() || time <= first.time {
        return Some(first.value);
    }
    let last = keys.last()?;
    if time >= last.time {
        return Some(last.value);
    }
    let next = keys.partition_point(|key| key.time <= time);
    let index = next - 1;
    let (from, to) = (&keys[index], &keys[next]);
    let span = to.time - from.time;
    if span <= 1e-9 {
        return Some(to.value);
    }
    let progress = ((time - from.time) / span).clamp(0.0, 1.0);
    let start = from.value.channels();
    let end = to.value.channels();
    let mut values = start;
    for (channel, value) in values.as_mut().iter_mut().enumerate() {
        let (a, b) = (start.as_ref()[channel], end.as_ref()[channel]);
        *value = match from.interpolation {
            KeyframeInterpolation::Hold => a,
            KeyframeInterpolation::Linear => a + (b - a) * progress,
            KeyframeInterpolation::Smooth => {
                let t = progress;
                let t2 = t * t;
                let t3 = t2 * t;
                (2.0 * t3 - 3.0 * t2 + 1.0) * a
                    + (t3 - 2.0 * t2 + t) * span * tangent(&keys, index, channel)
                    + (-2.0 * t3 + 3.0 * t2) * b
                    + (t3 - t2) * span * tangent(&keys, next, channel)
            }
        };
    }
    Some(V::from_channels(values))
}

pub fn sample_layer_keyframes(keys: &[LayerKeyframe], time: f64) -> Option<LayerPose> {
    sample_keyframes(keys, time)
}

pub fn keyframe_index_at<V>(keys: &[Keyframe<V>], time: f64, tolerance: f64) -> Option<usize> {
    keys.iter()
        .enumerate()
        .filter(|(_, key)| (key.time - time).abs() <= tolerance)
        .min_by(|(_, a), (_, b)| (a.time - time).abs().total_cmp(&(b.time - time).abs()))
        .map(|(index, _)| index)
}

/// Writes `value` at `time`: onto the keyframe already there, or as a new one
/// that continues the interpolation of the span it lands in. Returns its index.
pub fn upsert_keyframe<V: Copy>(
    keys: &mut Vec<Keyframe<V>>,
    time: f64,
    value: V,
    tolerance: f64,
) -> usize {
    keys.sort_by(|a, b| a.time.total_cmp(&b.time));
    if let Some(index) = keyframe_index_at(keys, time, tolerance) {
        keys[index].value = value;
        return index;
    }
    let index = keys.partition_point(|key| key.time <= time);
    let interpolation = index
        .checked_sub(1)
        .and_then(|previous| keys.get(previous))
        .or(keys.first())
        .map(|key| key.interpolation)
        .unwrap_or_default();
    keys.insert(
        index,
        Keyframe {
            time,
            value,
            interpolation,
        },
    );
    index
}

/// The nearest keyframe time strictly before or after `time`.
pub fn neighbouring_keyframe_time<V>(
    keys: &[Keyframe<V>],
    time: f64,
    tolerance: f64,
    forward: bool,
) -> Option<f64> {
    let times = keys.iter().map(|key| key.time);
    if forward {
        times
            .filter(|key| *key > time + tolerance)
            .min_by(f64::total_cmp)
    } else {
        times
            .filter(|key| *key < time - tolerance)
            .max_by(f64::total_cmp)
    }
}

/// A timeline segment whose look or level can be keyframed. Everything here
/// is about *when* the keyframes are, so one editor can drive any segment;
/// the value each kind animates is read and written through its own
/// `*_at` methods.
pub trait Keyframed {
    /// The segment's start and end on the timeline.
    fn keyframe_span(&self) -> (f64, f64);
    /// Every keyframe time, in seconds from the segment start, in stored order.
    fn keyframe_times(&self) -> Vec<f64>;
    fn keyframe_interpolation(&self, index: usize) -> Option<KeyframeInterpolation>;
    fn set_keyframe_interpolation(
        &mut self,
        index: usize,
        interpolation: KeyframeInterpolation,
    ) -> bool;
    /// Keys the current value at `time`. Returns false when one is already there.
    fn add_keyframe_at(&mut self, time: f64, tolerance: f64) -> bool;
    /// Removes one keyframe. Removing the last leaves the segment still, on
    /// the value it had at `time`.
    fn remove_keyframe(&mut self, index: usize, time: f64) -> bool;
    /// Drops all animation, keeping the value shown at `time`.
    fn clear_keyframes(&mut self, time: f64) -> bool;
    /// Moves one keyframe to a timeline time, kept inside the segment and off
    /// its neighbours. Returns its index after the move.
    fn retime_keyframe(&mut self, index: usize, time: f64, min_gap: f64) -> Option<usize>;
    /// Moves the segment's start edge while its animation stays where it was
    /// on the timeline. Keyframes left outside the segment keep shaping the
    /// animation inside it, so a trim or a split never changes the rest.
    fn trim_start_keeping_motion(&mut self, start: f64);

    fn has_keyframes(&self) -> bool {
        !self.keyframe_times().is_empty()
    }

    fn keyframe_index_at(&self, time: f64, tolerance: f64) -> Option<usize> {
        let local = time - self.keyframe_span().0;
        self.keyframe_times()
            .into_iter()
            .enumerate()
            .filter(|(_, key)| (key - local).abs() <= tolerance)
            .min_by(|(_, a), (_, b)| (a - local).abs().total_cmp(&(b - local).abs()))
            .map(|(index, _)| index)
    }

    /// Timeline time of the nearest keyframe before or after `time`.
    fn neighbouring_keyframe(&self, time: f64, tolerance: f64, forward: bool) -> Option<f64> {
        let (start, end) = self.keyframe_span();
        let local = time - start;
        let times = self.keyframe_times().into_iter();
        let found = if forward {
            times
                .filter(|key| *key > local + tolerance)
                .min_by(f64::total_cmp)
        } else {
            times
                .filter(|key| *key < local - tolerance)
                .max_by(f64::total_cmp)
        };
        found
            .map(|local| start + local)
            .filter(|time| (start - 1e-6..=end + 1e-6).contains(time))
    }

    /// Keyframe times that fall inside the segment, as seconds from its start.
    fn visible_keyframe_times(&self) -> Vec<f64> {
        let (start, end) = self.keyframe_span();
        let duration = (end - start).max(0.0);
        self.keyframe_times()
            .into_iter()
            .filter(|time| (-1e-6..=duration + 1e-6).contains(time))
            .collect()
    }
}

/// Implements [`Keyframed`] and the typed `$read`/`$write` pair for a segment
/// with `start`, `end`, a `$keys` keyframe list and a base value that
/// `$base`/`$set_base` read and write. The base mirrors the opening value so
/// builds that predate keyframes still show the segment sensibly.
macro_rules! keyframed_segment {
    (
        $segment:ty, $value:ty, $keys:ident,
        base: $base:expr, set_base: $set_base:expr,
        read: $read:ident, write: $write:ident
    ) => {
        impl $segment {
            fn keyframe_base(&self) -> $value {
                let base: fn(&Self) -> $value = $base;
                base(self)
            }

            fn set_keyframe_base(&mut self, value: $value) {
                let set: fn(&mut Self, $value) = $set_base;
                set(self, value)
            }

            fn mirror_opening_value(&mut self) {
                if let Some(value) = sample_keyframes(&self.$keys, 0.0) {
                    self.set_keyframe_base(value);
                }
            }

            fn keyframe_local_time(&self, time: f64) -> f64 {
                (time - self.start).clamp(0.0, (self.end - self.start).max(0.0))
            }

            /// The value at a timeline time, keyframed or not.
            pub fn $read(&self, time: f64) -> $value {
                sample_keyframes(&self.$keys, time - self.start)
                    .unwrap_or_else(|| self.keyframe_base())
            }

            /// Applies an edit made at `time`. A still segment just takes the
            /// value; an animated one gets it as a keyframe there, so editing
            /// never needs a separate "record" step once one keyframe exists.
            pub fn $write(&mut self, time: f64, value: $value, tolerance: f64) {
                if !value.channels().as_ref().iter().all(|v| v.is_finite()) {
                    return;
                }
                if self.$keys.is_empty() {
                    self.set_keyframe_base(value);
                    return;
                }
                let local = self.keyframe_local_time(time);
                upsert_keyframe(&mut self.$keys, local, value, tolerance);
                self.mirror_opening_value();
            }
        }

        impl Keyframed for $segment {
            fn keyframe_span(&self) -> (f64, f64) {
                (self.start, self.end)
            }

            fn keyframe_times(&self) -> Vec<f64> {
                self.$keys.iter().map(|key| key.time).collect()
            }

            fn keyframe_interpolation(&self, index: usize) -> Option<KeyframeInterpolation> {
                self.$keys.get(index).map(|key| key.interpolation)
            }

            fn set_keyframe_interpolation(
                &mut self,
                index: usize,
                interpolation: KeyframeInterpolation,
            ) -> bool {
                match self.$keys.get_mut(index) {
                    Some(key) if key.interpolation != interpolation => {
                        key.interpolation = interpolation;
                        true
                    }
                    _ => false,
                }
            }

            fn add_keyframe_at(&mut self, time: f64, tolerance: f64) -> bool {
                if self.keyframe_index_at(time, tolerance).is_some() {
                    return false;
                }
                let value = self.$read(time);
                let local = self.keyframe_local_time(time);
                upsert_keyframe(&mut self.$keys, local, value, tolerance);
                self.mirror_opening_value();
                true
            }

            fn remove_keyframe(&mut self, index: usize, time: f64) -> bool {
                if index >= self.$keys.len() {
                    return false;
                }
                let value = self.$read(time);
                self.$keys.remove(index);
                if self.$keys.is_empty() {
                    self.set_keyframe_base(value);
                } else {
                    self.mirror_opening_value();
                }
                true
            }

            fn clear_keyframes(&mut self, time: f64) -> bool {
                if self.$keys.is_empty() {
                    return false;
                }
                let value = self.$read(time);
                self.$keys.clear();
                self.set_keyframe_base(value);
                true
            }

            fn retime_keyframe(&mut self, index: usize, time: f64, min_gap: f64) -> Option<usize> {
                if index >= self.$keys.len() {
                    return None;
                }
                self.$keys.sort_by(|a, b| a.time.total_cmp(&b.time));
                let duration = (self.end - self.start).max(0.0);
                let low = index
                    .checked_sub(1)
                    .map_or(0.0, |previous| self.$keys[previous].time + min_gap);
                let high = self
                    .$keys
                    .get(index + 1)
                    .map_or(duration, |next| next.time - min_gap);
                if high < low {
                    return Some(index);
                }
                self.$keys[index].time = (time - self.start).clamp(low, high);
                self.mirror_opening_value();
                Some(index)
            }

            fn trim_start_keeping_motion(&mut self, start: f64) {
                let shift = self.start - start;
                for key in &mut self.$keys {
                    key.time += shift;
                }
                self.start = start;
                self.mirror_opening_value();
            }
        }
    };
}

keyframed_segment!(
    crate::ImageSegment, LayerPose, keyframes,
    base: |segment| LayerPose {
        center: segment.center,
        size: segment.size,
        rotation: segment.rotation,
        opacity: segment.opacity,
    },
    set_base: |segment, pose| {
        segment.center = pose.center;
        segment.size = pose.size;
        segment.rotation = pose.rotation;
        segment.opacity = pose.opacity;
    },
    read: pose_at, write: write_pose_at
);

keyframed_segment!(
    crate::TextSegment, TextPose, keyframes,
    base: |segment| TextPose {
        center: segment.center,
        size: segment.size,
        opacity: segment.opacity,
        font_size: segment.font_size,
    },
    set_base: |segment, pose| {
        segment.center = pose.center;
        segment.size = pose.size;
        segment.opacity = pose.opacity;
        segment.font_size = pose.font_size;
    },
    read: pose_at, write: write_pose_at
);

// `motion` rather than `keyframes` because that name holds the older
// per-property lists, which still play when `motion` is empty.
keyframed_segment!(
    crate::MaskSegment, MaskPose, motion,
    base: |segment| MaskPose {
        center: segment.center,
        size: segment.size,
        opacity: segment.opacity,
        pixelation: segment.pixelation,
        darkness: segment.darkness,
    },
    set_base: |segment, pose| {
        segment.center = pose.center;
        segment.size = pose.size;
        segment.opacity = pose.opacity;
        segment.pixelation = pose.pixelation;
        segment.darkness = pose.darkness;
    },
    read: pose_at, write: write_pose_at
);

// A shape is two points; its pose keeps their direction in a signed size so
// an arrow still points the way it was drawn.
keyframed_segment!(
    crate::ShapeSegment, LayerPose, keyframes,
    base: |segment| LayerPose {
        center: XY::new(
            (segment.from.x + segment.to.x) / 2.0,
            (segment.from.y + segment.to.y) / 2.0,
        ),
        size: XY::new(segment.to.x - segment.from.x, segment.to.y - segment.from.y),
        rotation: 0.0,
        opacity: segment.opacity,
    },
    set_base: |segment, pose| {
        segment.from = XY::new(
            pose.center.x - pose.size.x / 2.0,
            pose.center.y - pose.size.y / 2.0,
        );
        segment.to = XY::new(
            pose.center.x + pose.size.x / 2.0,
            pose.center.y + pose.size.y / 2.0,
        );
        segment.opacity = pose.opacity;
    },
    read: pose_at, write: write_pose_at
);

keyframed_segment!(
    crate::AudioTrackSegment, VolumeLevel, volume_keyframes,
    base: |segment| VolumeLevel {
        volume_db: segment.volume_db,
    },
    set_base: |segment, level| segment.volume_db = level.volume_db,
    read: volume_at, write: write_volume_at
);

/// A copy of the segment frozen at `time`, for code that reads its plain
/// fields; the segment itself when it has no keyframes.
macro_rules! resolved_at {
    ($segment:ty, $keys:ident $(, $clear:ident)?) => {
        impl $segment {
            pub fn resolved_at(&self, time: f64) -> std::borrow::Cow<'_, Self> {
                if self.$keys.is_empty() {
                    return std::borrow::Cow::Borrowed(self);
                }
                let value = self.pose_at(time);
                let mut still = self.clone();
                still.$keys.clear();
                $(still.$clear = Default::default();)?
                still.set_keyframe_base(value);
                std::borrow::Cow::Owned(still)
            }

            /// Runs an edit written against the plain fields as if the
            /// segment were still at `time`, then keeps whatever it did to
            /// the animated ones as a keyframe there.
            pub fn edit_at<R>(
                &mut self,
                time: f64,
                tolerance: f64,
                change: impl FnOnce(&mut Self) -> R,
            ) -> R {
                if self.$keys.is_empty() {
                    return change(self);
                }
                let mut still = self.resolved_at(time).into_owned();
                let result = change(&mut still);
                let value = still.keyframe_base();
                still.$keys = std::mem::take(&mut self.$keys);
                *self = still;
                self.write_pose_at(time, value, tolerance);
                result
            }
        }
    };
}

resolved_at!(crate::TextSegment, keyframes);
resolved_at!(crate::ShapeSegment, keyframes);
resolved_at!(crate::MaskSegment, motion, keyframes);

impl crate::ShapeSegment {
    /// The two points at a timeline time.
    pub fn points_at(&self, time: f64) -> (XY<f64>, XY<f64>) {
        let pose = self.pose_at(time);
        (
            XY::new(
                pose.center.x - pose.size.x / 2.0,
                pose.center.y - pose.size.y / 2.0,
            ),
            XY::new(
                pose.center.x + pose.size.x / 2.0,
                pose.center.y + pose.size.y / 2.0,
            ),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(time: f64, x: f64) -> LayerKeyframe {
        LayerKeyframe {
            time,
            value: LayerPose {
                center: XY::new(x, 0.5),
                size: XY::new(0.2, 0.2),
                rotation: 0.0,
                opacity: 1.0,
            },
            interpolation: KeyframeInterpolation::Smooth,
        }
    }

    fn x_at(keys: &[LayerKeyframe], time: f64) -> f64 {
        sample_layer_keyframes(keys, time).unwrap().center.x
    }

    #[test]
    fn no_keyframes_sample_to_nothing_and_one_holds_everywhere() {
        assert!(sample_layer_keyframes(&[], 1.0).is_none());
        let keys = [key(2.0, 0.3)];
        for time in [-1.0, 0.0, 2.0, 9.0, f64::NAN] {
            assert_eq!(x_at(&keys, time), 0.3);
        }
    }

    #[test]
    fn two_smooth_keyframes_ease_in_and_out() {
        let keys = [key(0.0, 0.0), key(2.0, 1.0)];
        assert_eq!(x_at(&keys, 0.0), 0.0);
        assert_eq!(x_at(&keys, 2.0), 1.0);
        assert!((x_at(&keys, 1.0) - 0.5).abs() < 1e-9);
        assert!(x_at(&keys, 0.2) < 0.1);
        assert!(x_at(&keys, 1.8) > 0.9);
    }

    #[test]
    fn smooth_motion_passes_through_every_keyframe_without_overshoot() {
        let keys = [
            key(0.0, 0.1),
            key(0.4, 0.8),
            key(0.5, 0.82),
            key(3.0, 0.9),
            key(3.2, 0.2),
            key(5.0, 0.2),
        ];
        for pair in keys.windows(2) {
            let (low, high) = (
                pair[0].value.center.x.min(pair[1].value.center.x),
                pair[0].value.center.x.max(pair[1].value.center.x),
            );
            for step in 0..=200 {
                let time = pair[0].time + (pair[1].time - pair[0].time) * f64::from(step) / 200.0;
                let value = x_at(&keys, time);
                assert!(
                    value >= low - 1e-9 && value <= high + 1e-9,
                    "{value} left [{low}, {high}] at {time}"
                );
            }
            assert!((x_at(&keys, pair[0].time) - pair[0].value.center.x).abs() < 1e-9);
        }
    }

    #[test]
    fn speed_carries_through_a_keyframe_between_two_moves_the_same_way() {
        let keys = [key(0.0, 0.0), key(1.0, 0.5), key(2.0, 1.0)];
        let step = 1e-4;
        let before = (x_at(&keys, 1.0) - x_at(&keys, 1.0 - step)) / step;
        let after = (x_at(&keys, 1.0 + step) - x_at(&keys, 1.0)) / step;
        assert!(before > 0.3 && (before - after).abs() < 1e-2);
    }

    #[test]
    fn linear_and_hold_spans_follow_their_leaving_keyframe() {
        let mut keys = [key(0.0, 0.0), key(1.0, 1.0), key(2.0, 0.0)];
        keys[0].interpolation = KeyframeInterpolation::Linear;
        keys[1].interpolation = KeyframeInterpolation::Hold;
        assert!((x_at(&keys, 0.25) - 0.25).abs() < 1e-9);
        assert_eq!(x_at(&keys, 1.5), 1.0);
        assert_eq!(x_at(&keys, 1.999), 1.0);
        assert_eq!(x_at(&keys, 2.0), 0.0);
    }

    #[test]
    fn unsorted_and_non_finite_keyframes_do_not_change_the_result() {
        let ordered = [key(0.0, 0.0), key(1.0, 0.6), key(2.0, 1.0)];
        let shuffled = [ordered[2], key(f64::NAN, 0.4), ordered[0], ordered[1]];
        for step in 0..=20 {
            let time = f64::from(step) / 10.0;
            assert_eq!(x_at(&ordered, time), x_at(&shuffled, time));
        }
    }

    #[test]
    fn writing_at_a_keyframe_replaces_it_and_elsewhere_inserts_in_order() {
        let mut keys = vec![key(0.0, 0.0), key(2.0, 1.0)];
        keys[0].interpolation = KeyframeInterpolation::Linear;
        assert_eq!(
            upsert_keyframe(&mut keys, 2.005, key(0.0, 0.7).value, 0.01),
            1
        );
        assert_eq!(keys.len(), 2);
        assert_eq!(keys[1].time, 2.0);
        assert_eq!(keys[1].value.center.x, 0.7);
        assert_eq!(
            upsert_keyframe(&mut keys, 1.0, key(0.0, 0.2).value, 0.01),
            1
        );
        assert_eq!(
            keys.iter().map(|key| key.time).collect::<Vec<_>>(),
            [0.0, 1.0, 2.0]
        );
        assert_eq!(keys[1].interpolation, KeyframeInterpolation::Linear);
    }

    #[test]
    fn neighbours_skip_the_keyframe_under_the_playhead() {
        let keys = [key(0.0, 0.0), key(1.0, 0.5), key(2.0, 1.0)];
        assert_eq!(
            neighbouring_keyframe_time(&keys, 1.0, 0.01, true),
            Some(2.0)
        );
        assert_eq!(
            neighbouring_keyframe_time(&keys, 1.0, 0.01, false),
            Some(0.0)
        );
        assert_eq!(neighbouring_keyframe_time(&keys, 2.0, 0.01, true), None);
        assert_eq!(
            neighbouring_keyframe_time(&keys, 0.5, 0.01, false),
            Some(0.0)
        );
    }

    fn picture() -> crate::ImageSegment {
        crate::ImageSegment {
            start: 10.0,
            end: 14.0,
            center: XY::new(0.2, 0.5),
            size: XY::new(0.2, 0.2),
            ..Default::default()
        }
    }

    #[test]
    fn a_still_picture_takes_edits_directly_and_an_animated_one_keys_them() {
        let mut layer = picture();
        let mut pose = layer.pose_at(11.0);
        pose.center.x = 0.4;
        layer.write_pose_at(11.0, pose, 0.01);
        assert!(layer.keyframes.is_empty());
        assert_eq!(layer.center.x, 0.4);

        assert!(layer.add_keyframe_at(10.0, 0.01));
        assert!(!layer.add_keyframe_at(10.0, 0.01));
        pose.center.x = 0.8;
        layer.write_pose_at(12.0, pose, 0.01);
        assert_eq!(
            layer
                .keyframes
                .iter()
                .map(|key| key.time)
                .collect::<Vec<_>>(),
            [0.0, 2.0]
        );
        assert_eq!(layer.center.x, 0.4);
        assert_eq!(layer.pose_at(9.0).center.x, 0.4);
        assert_eq!(layer.pose_at(13.0).center.x, 0.8);
        assert!((layer.pose_at(11.0).center.x - 0.6).abs() < 1e-9);
        assert_eq!(layer.neighbouring_keyframe(10.0, 0.01, true), Some(12.0));
    }

    #[test]
    fn removing_the_last_keyframe_freezes_the_pose_on_screen() {
        let mut layer = picture();
        layer.add_keyframe_at(10.0, 0.01);
        let mut pose = layer.pose_at(12.0);
        pose.center.x = 0.8;
        layer.write_pose_at(12.0, pose, 0.01);
        assert!(layer.remove_keyframe(0, 12.0));
        assert_eq!(layer.center.x, 0.8);
        assert!(layer.remove_keyframe(0, 12.0));
        assert!(layer.keyframes.is_empty());
        assert_eq!(layer.center.x, 0.8);
        assert!(!layer.clear_keyframes(12.0));
    }

    #[test]
    fn trimming_the_start_leaves_the_motion_in_place() {
        let mut layer = picture();
        layer.add_keyframe_at(10.0, 0.01);
        let mut pose = layer.pose_at(14.0);
        pose.center.x = 0.8;
        layer.write_pose_at(14.0, pose, 0.01);
        let before: Vec<f64> = (0..=8)
            .map(|step| layer.pose_at(10.0 + f64::from(step) * 0.5).center.x)
            .collect();

        let mut trimmed = layer.clone();
        trimmed.trim_start_keeping_motion(11.0);
        assert_eq!(trimmed.center.x, trimmed.pose_at(11.0).center.x);
        assert_eq!(trimmed.visible_keyframe_times(), [3.0]);
        for (step, expected) in before.iter().enumerate() {
            let time = 10.0 + step as f64 * 0.5;
            if time >= 11.0 {
                assert!((trimmed.pose_at(time).center.x - expected).abs() < 1e-9);
            }
        }
    }

    #[test]
    fn a_keyframe_cannot_be_dragged_past_its_neighbours_or_the_layer() {
        let mut layer = picture();
        for time in [10.0, 12.0, 14.0] {
            layer.add_keyframe_at(time, 0.01);
        }
        layer.retime_keyframe(1, 20.0, 0.05);
        assert!((layer.keyframes[1].time - 3.95).abs() < 1e-9);
        layer.retime_keyframe(2, 99.0, 0.05);
        assert_eq!(layer.keyframes[2].time, 4.0);
        layer.retime_keyframe(0, 0.0, 0.05);
        assert_eq!(layer.keyframes[0].time, 0.0);
    }

    #[test]
    fn old_projects_load_without_keyframes_and_still_ones_do_not_write_them() {
        let layer: crate::ImageSegment = serde_json::from_value(serde_json::json!({
            "start": 1.0, "end": 2.0, "path": "a.png"
        }))
        .unwrap();
        assert!(layer.keyframes.is_empty());
        assert!(
            serde_json::to_value(&layer)
                .unwrap()
                .get("keyframes")
                .is_none()
        );
    }

    #[test]
    fn keyframes_serialize_flat_and_read_back_with_defaults() {
        let json = serde_json::to_value(key(1.0, 0.25)).unwrap();
        assert_eq!(json["center"]["x"], 0.25);
        assert_eq!(json["interpolation"], "smooth");
        let read: LayerKeyframe = serde_json::from_value(serde_json::json!({
            "time": 1.0,
            "center": { "x": 0.25, "y": 0.5 },
            "size": { "x": 0.2, "y": 0.2 }
        }))
        .unwrap();
        assert_eq!(read, key(1.0, 0.25));
    }

    #[test]
    fn a_shape_keeps_its_direction_through_keyframes() {
        let mut arrow = crate::ShapeSegment {
            start: 0.0,
            end: 2.0,
            from: XY::new(0.8, 0.2),
            to: XY::new(0.6, 0.4),
            ..Default::default()
        };
        assert!(arrow.add_keyframe_at(0.0, 0.01));
        let mut pose = arrow.pose_at(2.0);
        pose.center.x -= 0.4;
        arrow.write_pose_at(2.0, pose, 0.01);
        let (from, to) = arrow.points_at(2.0);
        assert!((from.x - 0.4).abs() < 1e-9 && (to.x - 0.2).abs() < 1e-9);
        assert!(from.x > to.x && from.y < to.y);
        assert!((arrow.from.x - 0.8).abs() < 1e-9 && (arrow.to.x - 0.6).abs() < 1e-9);
    }

    #[test]
    fn volume_keyframes_fade_between_levels_and_store_only_what_they_need() {
        let mut music: crate::AudioTrackSegment = serde_json::from_value(serde_json::json!({
            "start": 4.0, "end": 8.0, "path": "music.mp3"
        }))
        .unwrap();
        assert!(music.volume_keyframes.is_empty());
        assert_eq!(music.volume_at(5.0).volume_db, 0.0);
        music.write_volume_at(5.0, VolumeLevel { volume_db: -3.0 }, 0.01);
        assert!(music.volume_keyframes.is_empty());
        assert!(music.add_keyframe_at(4.0, 0.01));
        music.write_volume_at(8.0, VolumeLevel { volume_db: -23.0 }, 0.01);
        assert_eq!(music.volume_db, -3.0);
        assert!((music.volume_at(6.0).volume_db + 13.0).abs() < 1e-4);
        let json = serde_json::to_value(&music).unwrap();
        assert_eq!(
            json["volumeKeyframes"][1],
            serde_json::json!({ "time": 4.0, "volumeDb": -23.0, "interpolation": "smooth" })
        );
    }

    #[test]
    fn text_size_and_mask_strength_animate_and_edits_at_a_time_become_keyframes() {
        let mut title = crate::default_text_segment(0.0, 2.0, 0);
        title.font_size = 40.0;
        assert!(title.add_keyframe_at(0.0, 0.01));
        title.edit_at(2.0, 0.01, |title| {
            title.font_size = 80.0;
            title.color = "#ff0000".to_string();
        });
        assert_eq!(title.keyframes.len(), 2);
        assert_eq!(title.font_size, 40.0);
        assert_eq!(title.color, "#ff0000");
        assert!((title.pose_at(1.0).font_size - 60.0).abs() < 1e-4);
        assert_eq!(title.resolved_at(2.0).font_size, 80.0);

        let mut blur = crate::default_mask_segment(0.0, 2.0, 0);
        let start = blur.pixelation;
        blur.darkness = 0.0;
        assert!(blur.add_keyframe_at(0.0, 0.01));
        blur.edit_at(2.0, 0.01, |blur| {
            blur.pixelation = start + 20.0;
            blur.darkness = 1.0;
        });
        let middle = blur.pose_at(1.0);
        assert!((middle.pixelation - (start + 10.0)).abs() < 1e-9);
        assert!((middle.darkness - 0.5).abs() < 1e-9);
        assert_eq!(blur.pixelation, start);
    }

    #[test]
    fn a_mask_with_new_motion_ignores_nothing_it_did_before() {
        let mut mask = crate::default_mask_segment(1.0, 3.0, 0);
        let still = mask.pose_at(2.0);
        assert_eq!(still.center, mask.center);
        assert!(mask.add_keyframe_at(1.0, 0.01));
        let mut pose = still;
        pose.center.x = 0.9;
        mask.write_pose_at(3.0, pose, 0.01);
        assert_eq!(mask.motion.len(), 2);
        assert!(mask.pose_at(2.0).center.x > still.center.x);
        assert_eq!(mask.center, still.center);
    }
}
