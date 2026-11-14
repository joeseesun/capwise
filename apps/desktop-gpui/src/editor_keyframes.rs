use cap_project::{
    ImageSegment, KeyframeInterpolation, Keyframed, LayerPose, TimelineConfiguration,
};
use gpui::{
    AnyElement, Context, Hsla, InteractiveElement, IntoElement, ParentElement, SharedString,
    StatefulInteractiveElement, Styled, Window, div, prelude::FluentBuilder, px,
};

use crate::{
    editor_timeline::{self as timeline, TrackKind},
    editor_window::{EDITOR_PREVIEW_FPS, EditorWindow},
    i18n::tr,
    ui,
};

const FRAME: f64 = 1.0 / EDITOR_PREVIEW_FPS as f64;
/// The playhead counts as "on" a keyframe within half a preview frame of it.
pub(crate) const KEYFRAME_TOLERANCE: f64 = FRAME / 2.0;
pub(crate) const KEYFRAME_MIN_GAP: f64 = FRAME;
const MARK_HIT_PX: f64 = 7.0;

const INTERPOLATIONS: [KeyframeInterpolation; 3] = [
    KeyframeInterpolation::Smooth,
    KeyframeInterpolation::Linear,
    KeyframeInterpolation::Hold,
];

pub(crate) fn keyframed(
    timeline: &TimelineConfiguration,
    kind: TrackKind,
    index: usize,
) -> Option<&dyn Keyframed> {
    Some(match kind {
        TrackKind::Image => timeline.image_segments.get(index)?,
        TrackKind::Text => timeline
            .text_segments
            .get(index)
            .filter(|segment| segment.title_card.is_none())?,
        TrackKind::Shape => timeline.shape_segments.get(index)?,
        TrackKind::Mask => timeline.mask_segments.get(index)?,
        TrackKind::Audio => timeline.audio_segments.get(index)?,
        _ => return None,
    })
}

pub(crate) fn keyframed_mut(
    timeline: &mut TimelineConfiguration,
    kind: TrackKind,
    index: usize,
) -> Option<&mut dyn Keyframed> {
    Some(match kind {
        TrackKind::Image => timeline.image_segments.get_mut(index)?,
        TrackKind::Text => timeline
            .text_segments
            .get_mut(index)
            .filter(|segment| segment.title_card.is_none())?,
        TrackKind::Shape => timeline.shape_segments.get_mut(index)?,
        TrackKind::Mask => timeline.mask_segments.get_mut(index)?,
        TrackKind::Audio => timeline.audio_segments.get_mut(index)?,
        _ => return None,
    })
}

/// The keyframe whose span covers `time`: the one at or before it, kept off
/// the last keyframe, which has no span after it to shape.
fn span_keyframe(segment: &dyn Keyframed, time: f64) -> Option<usize> {
    let times = segment.keyframe_times();
    let count = times.len();
    if count < 2 {
        return None;
    }
    let local = time - segment.keyframe_span().0;
    let mut order: Vec<usize> = (0..count).collect();
    order.sort_by(|a, b| times[*a].total_cmp(&times[*b]));
    let position = order
        .iter()
        .rposition(|index| times[*index] <= local + KEYFRAME_TOLERANCE)
        .unwrap_or(0)
        .min(count - 2);
    Some(order[position])
}

/// The nearest time inside the segment that a keyframe can sit on.
fn time_inside(segment: &dyn Keyframed, time: f64) -> f64 {
    let (start, end) = segment.keyframe_span();
    time.clamp(start, (end - FRAME).max(start))
}

impl EditorWindow {
    fn image_segment(&self, index: usize) -> Option<&ImageSegment> {
        self.project.timeline.as_ref()?.image_segments.get(index)
    }

    fn keyframed(&self, kind: TrackKind, index: usize) -> Option<&dyn Keyframed> {
        keyframed(self.project.timeline.as_ref()?, kind, index)
    }

    fn edit_keyframed(
        &mut self,
        reason: &'static str,
        kind: TrackKind,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
        change: impl FnOnce(&mut dyn Keyframed) -> bool,
    ) {
        self.edit_project(reason, window, cx, move |project| {
            project
                .timeline
                .as_mut()
                .and_then(|timeline| keyframed_mut(timeline, kind, index))
                .is_some_and(change)
        });
    }

    pub(crate) fn image_pose(&self, index: usize) -> Option<LayerPose> {
        Some(
            self.image_segment(index)?
                .pose_at(self.preview_or_playhead()),
        )
    }

    /// The keyframe diamond under a press at content-column `x`, as its index
    /// and its timeline time.
    pub(crate) fn keyframe_hit(
        &self,
        kind: TrackKind,
        index: usize,
        x: f64,
        secs_per_pixel: f64,
    ) -> Option<(usize, f64)> {
        let segment = self.keyframed(kind, index)?;
        let (start, end) = segment.keyframe_span();
        let duration = end - start;
        let left = (start - self.view.transform.position) / secs_per_pixel;
        let width = duration / secs_per_pixel;
        segment
            .keyframe_times()
            .into_iter()
            .enumerate()
            .filter(|(_, time)| (-1e-6..=duration + 1e-6).contains(time))
            .map(|(key_index, time)| {
                let mark = left + timeline::keyframe_mark_x(time, secs_per_pixel, width);
                (key_index, start + time, (mark - x).abs())
            })
            .filter(|(_, _, distance)| *distance <= MARK_HIT_PX)
            .min_by(|a, b| a.2.total_cmp(&b.2))
            .map(|(key_index, time, _)| (key_index, time))
    }

    fn toggle_keyframe(
        &mut self,
        kind: TrackKind,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(segment) = self.keyframed(kind, index) else {
            return;
        };
        let playhead = self.playhead_time();
        let time = time_inside(segment, playhead);
        if (time - playhead).abs() > KEYFRAME_TOLERANCE {
            self.seek_to_time(time, cx);
        }
        self.edit_keyframed(
            "keyframe",
            kind,
            index,
            window,
            cx,
            move |segment| match segment.keyframe_index_at(time, KEYFRAME_TOLERANCE) {
                Some(key) => segment.remove_keyframe(key, time),
                None => segment.add_keyframe_at(time, KEYFRAME_TOLERANCE),
            },
        );
    }

    fn jump_keyframe(
        &mut self,
        kind: TrackKind,
        index: usize,
        forward: bool,
        cx: &mut Context<Self>,
    ) {
        let playhead = self.playhead_time();
        if let Some(time) = self.keyframed(kind, index).and_then(|segment| {
            segment
                .neighbouring_keyframe(playhead, KEYFRAME_TOLERANCE, forward)
                .map(|time| time_inside(segment, time))
        }) {
            self.seek_to_time(time, cx);
        }
    }

    pub(crate) fn select_keyframe(&mut self, kind: TrackKind, index: usize, key: usize) {
        self.selected_keyframe = self
            .keyframed(kind, index)
            .and_then(|segment| segment.keyframe_times().get(key).copied())
            .map(|time| (kind, index, time));
    }

    /// Removes the keyframe picked on the timeline. Returns false when none
    /// is, so Delete can fall through to the selected segment.
    pub(crate) fn delete_selected_keyframe(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some((kind, index, local)) = self.selected_keyframe.take() else {
            return false;
        };
        let Some(key) = self.keyframed(kind, index).and_then(|segment| {
            segment
                .keyframe_times()
                .iter()
                .position(|time| (time - local).abs() < 1e-6)
        }) else {
            return false;
        };
        let time = self.preview_or_playhead();
        self.edit_keyframed("keyframe-delete", kind, index, window, cx, move |segment| {
            segment.remove_keyframe(key, time)
        });
        true
    }

    /// A scrubbed time pulled onto a keyframe of the selected segment when it
    /// passes within a few pixels, so going back to one to change it is easy.
    pub(crate) fn snap_to_keyframe(&self, time: f64, secs_per_pixel: f64) -> f64 {
        let Some(segment) = self
            .keyframe_target()
            .and_then(|(kind, index)| self.keyframed(kind, index))
        else {
            return time;
        };
        let (start, _) = segment.keyframe_span();
        let reach = MARK_HIT_PX * secs_per_pixel;
        segment
            .visible_keyframe_times()
            .into_iter()
            .map(|local| start + local)
            .filter(|key| (key - time).abs() <= reach)
            .min_by(|a, b| (a - time).abs().total_cmp(&(b - time).abs()))
            .map_or(time, |key| time_inside(segment, key))
    }

    /// A slider for an animated property with its keyframe diamond beside it,
    /// lit when the playhead is on a keyframe. Segments that cannot be
    /// keyframed get the slider alone.
    pub(crate) fn keyframe_slider(
        &self,
        kind: TrackKind,
        index: usize,
        name: &'static str,
        field: impl IntoElement,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(segment) = self.keyframed(kind, index) else {
            return field.into_any_element();
        };
        let theme = self.theme;
        let on_keyframe = segment
            .keyframe_index_at(self.playhead_time(), KEYFRAME_TOLERANCE)
            .is_some();
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(4.))
            .child(div().flex_1().min_w_0().child(field))
            .child(
                div()
                    .id(SharedString::from(format!(
                        "keyframe-mark-tip-{kind:?}-{index}-{name}"
                    )))
                    .flex_none()
                    .tooltip_show_delay(ui::TOOLTIP_SHOW_DELAY)
                    .tooltip(move |_, cx| {
                        ui::Tooltip::new(
                            &theme,
                            if on_keyframe {
                                tr::editor::panel::keyframes::remove()
                            } else {
                                tr::editor::panel::keyframes::add()
                            },
                        )
                        .keys(["K"])
                        .view(cx)
                    })
                    .child(
                        ui::EditorButton::plain(
                            &theme,
                            SharedString::from(format!("keyframe-mark-{kind:?}-{index}-{name}")),
                        )
                        .width(px(22.))
                        .height(px(22.))
                        .padding_x(px(0.))
                        .left_icon(if on_keyframe {
                            "icons/keyframe-filled.svg"
                        } else {
                            "icons/keyframe.svg"
                        })
                        .icon_size(px(11.))
                        .on_click(cx.listener(
                            move |this, _, window, cx| {
                                this.toggle_keyframe(kind, index, window, cx);
                            },
                        )),
                    ),
            )
            .into_any_element()
    }

    /// The one segment the keyframe button and shortcuts act on.
    pub(crate) fn keyframe_target(&self) -> Option<(TrackKind, usize)> {
        let selection = self.selection.as_ref()?;
        match selection.indices.as_slice() {
            [index] => self
                .keyframed(selection.track, *index)
                .map(|_| (selection.track, *index)),
            _ => None,
        }
    }

    /// `K`, `[` and `]`. Returns whether the key was taken.
    pub(crate) fn keyframe_shortcut(
        &mut self,
        key: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some((kind, index)) = self.keyframe_target() else {
            return false;
        };
        match key {
            "k" => self.toggle_keyframe(kind, index, window, cx),
            "[" => self.jump_keyframe(kind, index, false, cx),
            "]" => self.jump_keyframe(kind, index, true, cx),
            _ => return false,
        }
        true
    }

    pub(crate) fn render_keyframe_button(
        &self,
        compact: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = self.theme;
        let target = self.keyframe_target();
        let on_keyframe = target
            .and_then(|(kind, index)| self.keyframed(kind, index))
            .is_some_and(|segment| {
                segment
                    .keyframe_index_at(self.playhead_time(), KEYFRAME_TOLERANCE)
                    .is_some()
            });
        let tip = if on_keyframe {
            tr::editor::panel::keyframes::remove()
        } else {
            tr::editor::panel::keyframes::add()
        };
        let mut button = ui::Button::plain(
            &theme,
            "transport-keyframe",
            if on_keyframe {
                ui::ButtonVariant::Primary
            } else {
                ui::ButtonVariant::Gray
            },
            ui::ButtonSize::Sm,
        )
        .icon(if on_keyframe {
            "icons/keyframe-filled.svg"
        } else {
            "icons/keyframe.svg"
        })
        .radius(px(7.))
        .disabled(target.is_none() || !self.project_ready())
        .on_click(cx.listener(|this, _, window, cx| {
            if let Some((kind, index)) = this.keyframe_target() {
                this.toggle_keyframe(kind, index, window, cx);
            }
        }));
        if !compact {
            button = button.label(tr::editor::panel::keyframes::title());
        }
        div()
            .id("transport-keyframe-tooltip")
            .tooltip_show_delay(ui::TOOLTIP_SHOW_DELAY)
            .tooltip(move |_, cx| {
                ui::Tooltip::new(
                    &theme,
                    if target.is_some() {
                        tip
                    } else {
                        tr::editor::panel::keyframes::select_layer()
                    },
                )
                .keys(["K"])
                .view(cx)
            })
            .child(button)
            .into_any_element()
    }

    pub(crate) fn render_keyframes(
        &self,
        kind: TrackKind,
        index: usize,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(segment) = self.keyframed(kind, index) else {
            return div().into_any_element();
        };
        let all_times = segment.keyframe_times();
        let (segment_start, _) = segment.keyframe_span();
        let theme = self.theme;
        let playhead = self.playhead_time();
        let mut times = segment.visible_keyframe_times();
        times.sort_by(f64::total_cmp);
        let local = playhead - segment_start;
        let current = times
            .iter()
            .position(|time| (time - local).abs() <= KEYFRAME_TOLERANCE);
        let animated = !all_times.is_empty();
        let has_previous = segment
            .neighbouring_keyframe(playhead, KEYFRAME_TOLERANCE, false)
            .is_some();
        let has_next = segment
            .neighbouring_keyframe(playhead, KEYFRAME_TOLERANCE, true)
            .is_some();
        let span = span_keyframe(segment, playhead);
        let span_label = span.map(|key| {
            let from = times
                .iter()
                .position(|time| (time - all_times[key]).abs() < 1e-9)
                .map_or(1, |position| position + 1);
            SharedString::from(format!(
                "{} {from} → {}",
                tr::editor::panel::keyframes::transition(),
                from + 1
            ))
        });
        let interpolation = span.and_then(|key| segment.keyframe_interpolation(key));

        let step = |forward: bool, enabled: bool, cx: &mut Context<Self>| {
            let button = ui::EditorButton::plain(
                &theme,
                SharedString::from(format!("keyframe-{kind:?}-step-{index}-{forward}")),
            )
            .width(px(28.))
            .left_icon(if forward {
                "icons/chevron-right.svg"
            } else {
                "icons/chevron-left.svg"
            })
            .icon_size(px(14.))
            .disabled(!enabled)
            .on_click(cx.listener(move |this, _, _, cx| {
                this.jump_keyframe(kind, index, forward, cx);
            }));
            div()
                .id(SharedString::from(format!(
                    "keyframe-{kind:?}-step-tip-{index}-{forward}"
                )))
                .tooltip_show_delay(ui::TOOLTIP_SHOW_DELAY)
                .tooltip(move |_, cx| {
                    ui::Tooltip::new(
                        &theme,
                        if forward {
                            tr::editor::panel::keyframes::next()
                        } else {
                            tr::editor::panel::keyframes::previous()
                        },
                    )
                    .keys([if forward { "]" } else { "[" }])
                    .view(cx)
                })
                .child(button)
        };
        let on_keyframe = current.is_some();
        let toggle = div()
            .id(SharedString::from(format!(
                "keyframe-{kind:?}-toggle-tip-{index}"
            )))
            .tooltip_show_delay(ui::TOOLTIP_SHOW_DELAY)
            .tooltip(move |_, cx| {
                ui::Tooltip::new(
                    &theme,
                    if on_keyframe {
                        tr::editor::panel::keyframes::remove()
                    } else {
                        tr::editor::panel::keyframes::add()
                    },
                )
                .keys(["K"])
                .view(cx)
            })
            .child(
                ui::EditorButton::plain(
                    &theme,
                    SharedString::from(format!("keyframe-{kind:?}-toggle-{index}")),
                )
                .choice(&theme)
                .pressed(on_keyframe)
                .width(px(32.))
                .left_icon(if on_keyframe {
                    "icons/keyframe-filled.svg"
                } else {
                    "icons/keyframe.svg"
                })
                .icon_size(px(14.))
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.toggle_keyframe(kind, index, window, cx);
                })),
            );
        let count = SharedString::from(match current {
            Some(position) => format!("{} / {}", position + 1, times.len()),
            None => format!("– / {}", times.len()),
        });

        let controls = div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(2.))
            .when(animated, |row| row.child(step(false, has_previous, cx)))
            .child(toggle)
            .when(animated, |row| {
                row.child(step(true, has_next, cx)).child(
                    div()
                        .ml(px(6.))
                        .text_size(px(12.))
                        .font_features(timeline::tabular_numerals())
                        .text_color(Hsla::from(theme.editor.text_2))
                        .child(count),
                )
            })
            .child(div().flex_1())
            .when(animated, |row| {
                row.child(
                    ui::EditorButton::plain(
                        &theme,
                        SharedString::from(format!("keyframe-{kind:?}-clear-{index}")),
                    )
                    .text_size(px(12.))
                    .label(tr::editor::panel::keyframes::clear())
                    .on_click(cx.listener(move |this, _, window, cx| {
                        let time = this.preview_or_playhead();
                        this.edit_keyframed(
                            "keyframes-clear",
                            kind,
                            index,
                            window,
                            cx,
                            move |segment| segment.clear_keyframes(time),
                        );
                    })),
                )
            });

        ui::Field::stacked(&theme, tr::editor::panel::keyframes::title())
            .child(controls)
            .when(!animated, |field| {
                field.child(
                    div()
                        .text_size(px(12.))
                        .line_height(px(17.))
                        .text_color(Hsla::from(theme.editor.text_3))
                        .child(tr::editor::panel::keyframes::hint_empty()),
                )
            })
            .when_some(
                span.zip(interpolation).zip(span_label),
                |field, ((key, current), label)| {
                    field.child(
                        ui::Field::inline(&theme, label).child(
                            ui::SegmentedControl::editor(
                                &theme,
                                SharedString::from(format!("keyframe-{kind:?}-motion-{index}")),
                                INTERPOLATIONS
                                    .iter()
                                    .map(|value| {
                                        ui::SegmentOption::new(
                                            match value {
                                                KeyframeInterpolation::Smooth => {
                                                    tr::editor::panel::keyframes::smooth()
                                                }
                                                KeyframeInterpolation::Linear => {
                                                    tr::editor::panel::keyframes::linear()
                                                }
                                                KeyframeInterpolation::Hold => {
                                                    tr::editor::panel::keyframes::hold()
                                                }
                                            },
                                            *value == current,
                                        )
                                    })
                                    .collect(),
                            )
                            .on_select(cx.listener(
                                move |this, choice: &usize, window, cx| {
                                    let Some(value) = INTERPOLATIONS.get(*choice).copied() else {
                                        return;
                                    };
                                    this.edit_keyframed(
                                        "keyframe-motion",
                                        kind,
                                        index,
                                        window,
                                        cx,
                                        move |segment| {
                                            segment.set_keyframe_interpolation(key, value)
                                        },
                                    );
                                },
                            )),
                        ),
                    )
                },
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layer(times: &[f64]) -> ImageSegment {
        let mut layer = ImageSegment {
            start: 10.0,
            end: 14.0,
            ..Default::default()
        };
        for time in times {
            layer.add_keyframe_at(10.0 + time, KEYFRAME_TOLERANCE);
        }
        layer
    }

    #[test]
    fn the_span_under_the_playhead_is_never_the_last_keyframe() {
        assert_eq!(span_keyframe(&layer(&[1.0]), 11.0), None);
        let layer = layer(&[0.0, 1.0, 3.0]);
        assert_eq!(span_keyframe(&layer, 9.0), Some(0));
        assert_eq!(span_keyframe(&layer, 10.5), Some(0));
        assert_eq!(span_keyframe(&layer, 11.0), Some(1));
        assert_eq!(span_keyframe(&layer, 13.9), Some(1));
    }

    #[test]
    fn a_keyframe_always_lands_on_a_frame_the_layer_shows() {
        let layer = layer(&[]);
        assert_eq!(time_inside(&layer, 2.0), 10.0);
        assert_eq!(time_inside(&layer, 12.0), 12.0);
        assert!(layer.is_active_at(time_inside(&layer, 99.0)));
    }

    #[test]
    fn every_keyframed_kind_resolves_and_other_tracks_do_not() {
        let mut timeline: TimelineConfiguration = serde_json::from_value(serde_json::json!({
            "segments": [], "zoomSegments": []
        }))
        .unwrap();
        timeline.image_segments.push(layer(&[]));
        timeline
            .text_segments
            .push(cap_project::default_text_segment(0.0, 1.0, 0));
        timeline.shape_segments.push(Default::default());
        timeline
            .mask_segments
            .push(cap_project::default_mask_segment(0.0, 1.0, 0));
        for kind in [
            TrackKind::Image,
            TrackKind::Text,
            TrackKind::Shape,
            TrackKind::Mask,
        ] {
            assert!(keyframed(&timeline, kind, 0).is_some(), "{kind:?}");
            assert!(keyframed_mut(&mut timeline, kind, 0).is_some());
            assert!(keyframed(&timeline, kind, 1).is_none());
        }
        assert!(keyframed(&timeline, TrackKind::Zoom, 0).is_none());
    }
}
