use serde::{Deserialize, Serialize};
use specta::Type;

use crate::XY;

/// How a layer travels from one keyframe to the next. Stored on the keyframe
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

    fn channels(&self) -> [f64; CHANNELS] {
        [
            self.center.x,
            self.center.y,
            self.size.x,
            self.size.y,
            f64::from(self.rotation),
            f64::from(self.opacity),
        ]
    }

    fn from_channels(values: [f64; CHANNELS]) -> Self {
        Self {
            center: XY::new(values[0], values[1]),
            size: XY::new(values[2], values[3]),
            rotation: values[4] as f32,
            opacity: values[5] as f32,
        }
    }

    fn is_finite(&self) -> bool {
        self.channels().iter().all(|value| value.is_finite())
    }
}

const CHANNELS: usize = 6;

/// A whole-pose keyframe on a layer. `time` is seconds from the layer's own
/// start, so moving the layer on the timeline carries its motion with it.
#[derive(Type, Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct LayerKeyframe {
    pub time: f64,
    #[serde(flatten)]
    pub pose: LayerPose,
    #[serde(default)]
    pub interpolation: KeyframeInterpolation,
}

fn sorted(keys: &[LayerKeyframe]) -> std::borrow::Cow<'_, [LayerKeyframe]> {
    let usable = |key: &LayerKeyframe| key.time.is_finite() && key.pose.is_finite();
    if keys.iter().all(usable) && keys.is_sorted_by(|a, b| a.time <= b.time) {
        return std::borrow::Cow::Borrowed(keys);
    }
    let mut owned: Vec<_> = keys.iter().copied().filter(usable).collect();
    owned.sort_by(|a, b| a.time.total_cmp(&b.time));
    std::borrow::Cow::Owned(owned)
}

fn secant(keys: &[LayerKeyframe], from: usize, channel: usize) -> f64 {
    let span = keys[from + 1].time - keys[from].time;
    if span <= 1e-9 {
        return 0.0;
    }
    (keys[from + 1].pose.channels()[channel] - keys[from].pose.channels()[channel]) / span
}

/// The slope a smooth span uses at `index`: flat at the ends, at a turning
/// point and next to a hold; otherwise the monotone (Fritsch-Butland) mean of
/// the two neighbouring slopes, which is what keeps the curve from ever
/// passing a keyed value. Next to a linear span it takes that span's slope so
/// the speed does not jump, capped where it would overshoot.
fn tangent(keys: &[LayerKeyframe], index: usize, channel: usize) -> f64 {
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

/// The pose at `time` (seconds from the layer start), or `None` when the
/// layer has no usable keyframes. Holds before the first and after the last.
pub fn sample_layer_keyframes(keys: &[LayerKeyframe], time: f64) -> Option<LayerPose> {
    let keys = sorted(keys);
    let first = keys.first()?;
    if !time.is_finite() || time <= first.time {
        return Some(first.pose);
    }
    let last = keys.last()?;
    if time >= last.time {
        return Some(last.pose);
    }
    let next = keys.partition_point(|key| key.time <= time);
    let index = next - 1;
    let (from, to) = (&keys[index], &keys[next]);
    let span = to.time - from.time;
    if span <= 1e-9 {
        return Some(to.pose);
    }
    let progress = ((time - from.time) / span).clamp(0.0, 1.0);
    let start = from.pose.channels();
    let end = to.pose.channels();
    let mut values = [0.0; CHANNELS];
    for (channel, value) in values.iter_mut().enumerate() {
        *value = match from.interpolation {
            KeyframeInterpolation::Hold => start[channel],
            KeyframeInterpolation::Linear => {
                start[channel] + (end[channel] - start[channel]) * progress
            }
            KeyframeInterpolation::Smooth => {
                let t = progress;
                let t2 = t * t;
                let t3 = t2 * t;
                (2.0 * t3 - 3.0 * t2 + 1.0) * start[channel]
                    + (t3 - 2.0 * t2 + t) * span * tangent(&keys, index, channel)
                    + (-2.0 * t3 + 3.0 * t2) * end[channel]
                    + (t3 - t2) * span * tangent(&keys, next, channel)
            }
        };
    }
    Some(LayerPose::from_channels(values))
}

pub fn keyframe_index_at(keys: &[LayerKeyframe], time: f64, tolerance: f64) -> Option<usize> {
    keys.iter()
        .enumerate()
        .filter(|(_, key)| (key.time - time).abs() <= tolerance)
        .min_by(|(_, a), (_, b)| (a.time - time).abs().total_cmp(&(b.time - time).abs()))
        .map(|(index, _)| index)
}

/// Writes `pose` at `time`: onto the keyframe already there, or as a new one
/// that continues the interpolation of the span it lands in. Returns its index.
pub fn upsert_keyframe(
    keys: &mut Vec<LayerKeyframe>,
    time: f64,
    pose: LayerPose,
    tolerance: f64,
) -> usize {
    keys.sort_by(|a, b| a.time.total_cmp(&b.time));
    if let Some(index) = keyframe_index_at(keys, time, tolerance) {
        keys[index].pose = pose;
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
        LayerKeyframe {
            time,
            pose,
            interpolation,
        },
    );
    index
}

/// The nearest keyframe time strictly before or after `time`.
pub fn neighbouring_keyframe_time(
    keys: &[LayerKeyframe],
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

impl crate::ImageSegment {
    fn base_pose(&self) -> LayerPose {
        LayerPose {
            center: self.center,
            size: self.size,
            rotation: self.rotation,
            opacity: self.opacity,
        }
    }

    fn set_base_pose(&mut self, pose: LayerPose) {
        self.center = pose.center;
        self.size = pose.size;
        self.rotation = pose.rotation;
        self.opacity = pose.opacity;
    }

    fn mirror_opening_pose(&mut self) {
        if let Some(pose) = sample_layer_keyframes(&self.keyframes, 0.0) {
            self.set_base_pose(pose);
        }
    }

    fn local_time(&self, time: f64) -> f64 {
        (time - self.start).clamp(0.0, (self.end - self.start).max(0.0))
    }

    /// The picture's pose at a timeline time, keyframed or not.
    pub fn pose_at(&self, time: f64) -> LayerPose {
        sample_layer_keyframes(&self.keyframes, time - self.start)
            .unwrap_or_else(|| self.base_pose())
    }

    pub fn keyframe_index_at(&self, time: f64, tolerance: f64) -> Option<usize> {
        keyframe_index_at(&self.keyframes, time - self.start, tolerance)
    }

    /// Timeline time of the nearest keyframe before or after `time`.
    pub fn neighbouring_keyframe(&self, time: f64, tolerance: f64, forward: bool) -> Option<f64> {
        neighbouring_keyframe_time(&self.keyframes, time - self.start, tolerance, forward)
            .map(|local| self.start + local)
            .filter(|time| (self.start - 1e-6..=self.end + 1e-6).contains(time))
    }

    /// Applies an edit made at `time`. A still picture just takes the pose; an
    /// animated one gets it as a keyframe there, so editing never needs a
    /// separate "record" step once the first keyframe exists.
    pub fn write_pose_at(&mut self, time: f64, pose: LayerPose, tolerance: f64) {
        if !pose.is_finite() {
            return;
        }
        if self.keyframes.is_empty() {
            self.set_base_pose(pose);
            return;
        }
        let local = self.local_time(time);
        upsert_keyframe(&mut self.keyframes, local, pose, tolerance);
        self.mirror_opening_pose();
    }

    /// Keys the current pose at `time`. Returns false when one is already there.
    pub fn add_keyframe_at(&mut self, time: f64, tolerance: f64) -> bool {
        if self.keyframe_index_at(time, tolerance).is_some() {
            return false;
        }
        let pose = self.pose_at(time);
        let local = self.local_time(time);
        upsert_keyframe(&mut self.keyframes, local, pose, tolerance);
        self.mirror_opening_pose();
        true
    }

    /// Removes one keyframe. Removing the last leaves the picture still, in
    /// the pose it had at `time`.
    pub fn remove_keyframe(&mut self, index: usize, time: f64) -> bool {
        if index >= self.keyframes.len() {
            return false;
        }
        let pose = self.pose_at(time);
        self.keyframes.remove(index);
        if self.keyframes.is_empty() {
            self.set_base_pose(pose);
        } else {
            self.mirror_opening_pose();
        }
        true
    }

    /// Drops all motion, keeping the pose shown at `time`.
    pub fn clear_keyframes(&mut self, time: f64) -> bool {
        if self.keyframes.is_empty() {
            return false;
        }
        let pose = self.pose_at(time);
        self.keyframes.clear();
        self.set_base_pose(pose);
        true
    }

    pub fn set_keyframe_interpolation(
        &mut self,
        index: usize,
        interpolation: KeyframeInterpolation,
    ) -> bool {
        match self.keyframes.get_mut(index) {
            Some(key) if key.interpolation != interpolation => {
                key.interpolation = interpolation;
                true
            }
            _ => false,
        }
    }

    /// Moves one keyframe to a timeline time, kept inside the layer and off
    /// its neighbours. Returns its index after the move.
    pub fn retime_keyframe(&mut self, index: usize, time: f64, min_gap: f64) -> Option<usize> {
        if index >= self.keyframes.len() {
            return None;
        }
        self.keyframes.sort_by(|a, b| a.time.total_cmp(&b.time));
        let duration = (self.end - self.start).max(0.0);
        let low = index
            .checked_sub(1)
            .map_or(0.0, |previous| self.keyframes[previous].time + min_gap);
        let high = self
            .keyframes
            .get(index + 1)
            .map_or(duration, |next| next.time - min_gap);
        if high < low {
            return Some(index);
        }
        self.keyframes[index].time = (time - self.start).clamp(low, high);
        self.mirror_opening_pose();
        Some(index)
    }

    /// Moves the layer's start edge while its motion stays where it was on
    /// the timeline. Keyframes left outside the layer keep shaping the motion
    /// inside it, so a trim or a split never changes how the rest moves.
    pub fn trim_start_keeping_motion(&mut self, start: f64) {
        let shift = self.start - start;
        for key in &mut self.keyframes {
            key.time += shift;
        }
        self.start = start;
        self.mirror_opening_pose();
    }

    /// Keyframe times that fall inside the layer, as seconds from its start.
    pub fn visible_keyframe_times(&self) -> impl Iterator<Item = f64> + '_ {
        let duration = (self.end - self.start).max(0.0);
        self.keyframes
            .iter()
            .map(|key| key.time)
            .filter(move |time| (-1e-6..=duration + 1e-6).contains(time))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(time: f64, x: f64) -> LayerKeyframe {
        LayerKeyframe {
            time,
            pose: LayerPose {
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
                pair[0].pose.center.x.min(pair[1].pose.center.x),
                pair[0].pose.center.x.max(pair[1].pose.center.x),
            );
            for step in 0..=200 {
                let time = pair[0].time + (pair[1].time - pair[0].time) * f64::from(step) / 200.0;
                let value = x_at(&keys, time);
                assert!(
                    value >= low - 1e-9 && value <= high + 1e-9,
                    "{value} left [{low}, {high}] at {time}"
                );
            }
            assert!((x_at(&keys, pair[0].time) - pair[0].pose.center.x).abs() < 1e-9);
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
            upsert_keyframe(&mut keys, 2.005, key(0.0, 0.7).pose, 0.01),
            1
        );
        assert_eq!(keys.len(), 2);
        assert_eq!(keys[1].time, 2.0);
        assert_eq!(keys[1].pose.center.x, 0.7);
        assert_eq!(upsert_keyframe(&mut keys, 1.0, key(0.0, 0.2).pose, 0.01), 1);
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
        assert_eq!(trimmed.visible_keyframe_times().collect::<Vec<_>>(), [3.0]);
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
}
