use cap_project::{ImageSegment, KeyframeInterpolation, LayerPose};
use gpui::{
    AnyElement, Context, Hsla, IntoElement, ParentElement, SharedString, Styled, Window, div,
    prelude::FluentBuilder, px,
};

use crate::{
    editor_timeline as timeline,
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

/// The keyframe whose span covers `time`: the one at or before it, kept off
/// the last keyframe, which has no span after it to shape.
fn span_keyframe(segment: &ImageSegment, time: f64) -> Option<usize> {
    let count = segment.keyframes.len();
    if count < 2 {
        return None;
    }
    let local = time - segment.start;
    let mut order: Vec<usize> = (0..count).collect();
    order.sort_by(|a, b| {
        segment.keyframes[*a]
            .time
            .total_cmp(&segment.keyframes[*b].time)
    });
    let position = order
        .iter()
        .rposition(|index| segment.keyframes[*index].time <= local + KEYFRAME_TOLERANCE)
        .unwrap_or(0)
        .min(count - 2);
    Some(order[position])
}

/// The nearest time inside the layer that a keyframe can sit on.
fn time_inside(segment: &ImageSegment, time: f64) -> f64 {
    let last = (segment.end - FRAME).max(segment.start);
    time.clamp(segment.start, last)
}

impl EditorWindow {
    fn image_segment(&self, index: usize) -> Option<&ImageSegment> {
        self.project.timeline.as_ref()?.image_segments.get(index)
    }

    pub(crate) fn image_pose(&self, index: usize) -> Option<LayerPose> {
        Some(
            self.image_segment(index)?
                .pose_at(self.preview_or_playhead()),
        )
    }

    /// The keyframe diamond under a press at content-column `x`, as its index
    /// and its timeline time.
    pub(crate) fn image_keyframe_hit(
        &self,
        index: usize,
        x: f64,
        secs_per_pixel: f64,
    ) -> Option<(usize, f64)> {
        let segment = self.image_segment(index)?;
        let duration = segment.end - segment.start;
        let left = (segment.start - self.view.transform.position) / secs_per_pixel;
        let width = duration / secs_per_pixel;
        segment
            .keyframes
            .iter()
            .enumerate()
            .filter(|(_, key)| (-1e-6..=duration + 1e-6).contains(&key.time))
            .map(|(key_index, key)| {
                let mark = left + timeline::keyframe_mark_x(key.time, secs_per_pixel, width);
                (key_index, segment.start + key.time, (mark - x).abs())
            })
            .filter(|(_, _, distance)| *distance <= MARK_HIT_PX)
            .min_by(|a, b| a.2.total_cmp(&b.2))
            .map(|(key_index, time, _)| (key_index, time))
    }

    fn toggle_image_keyframe(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(segment) = self.image_segment(index) else {
            return;
        };
        let playhead = self.playhead_time();
        let time = time_inside(segment, playhead);
        if (time - playhead).abs() > KEYFRAME_TOLERANCE {
            self.seek_to_time(time, cx);
        }
        self.edit_image_segment(
            "image-keyframe",
            index,
            window,
            cx,
            move |segment| match segment.keyframe_index_at(time, KEYFRAME_TOLERANCE) {
                Some(key) => segment.remove_keyframe(key, time),
                None => segment.add_keyframe_at(time, KEYFRAME_TOLERANCE),
            },
        );
    }

    fn jump_image_keyframe(&mut self, index: usize, forward: bool, cx: &mut Context<Self>) {
        let playhead = self.playhead_time();
        if let Some(time) = self.image_segment(index).and_then(|segment| {
            segment
                .neighbouring_keyframe(playhead, KEYFRAME_TOLERANCE, forward)
                .map(|time| time_inside(segment, time))
        }) {
            self.seek_to_time(time, cx);
        }
    }

    pub(crate) fn render_image_keyframes(
        &self,
        index: usize,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(segment) = self.image_segment(index) else {
            return div().into_any_element();
        };
        let theme = self.theme;
        let playhead = self.playhead_time();
        let animated = !segment.keyframes.is_empty();
        let on_keyframe = segment
            .keyframe_index_at(playhead, KEYFRAME_TOLERANCE)
            .is_some();
        let has_previous = segment
            .neighbouring_keyframe(playhead, KEYFRAME_TOLERANCE, false)
            .is_some();
        let has_next = segment
            .neighbouring_keyframe(playhead, KEYFRAME_TOLERANCE, true)
            .is_some();
        let span = span_keyframe(segment, playhead);
        let interpolation = span.map(|key| segment.keyframes[key].interpolation);

        let step = |forward: bool, enabled: bool, cx: &mut Context<Self>| {
            ui::EditorButton::plain(
                &theme,
                SharedString::from(format!("image-keyframe-step-{index}-{forward}")),
            )
            .width(px(28.))
            .left_icon(if forward {
                "icons/chevron-right.svg"
            } else {
                "icons/chevron-left.svg"
            })
            .icon_size(px(14.))
            .disabled(!enabled)
            .tooltip(
                &theme,
                if forward {
                    tr::editor::panel::keyframes::next()
                } else {
                    tr::editor::panel::keyframes::previous()
                },
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                this.jump_image_keyframe(index, forward, cx);
            }))
        };
        let toggle = ui::EditorButton::plain(
            &theme,
            SharedString::from(format!("image-keyframe-toggle-{index}")),
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
        .tooltip(
            &theme,
            if on_keyframe {
                tr::editor::panel::keyframes::remove()
            } else {
                tr::editor::panel::keyframes::add()
            },
        )
        .on_click(cx.listener(move |this, _, window, cx| {
            this.toggle_image_keyframe(index, window, cx);
        }));

        let controls = div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(2.))
            .when(animated, |row| row.child(step(false, has_previous, cx)))
            .child(toggle)
            .when(animated, |row| row.child(step(true, has_next, cx)));

        div()
            .flex()
            .flex_col()
            .gap(px(10.))
            .child(
                ui::Field::inline(&theme, tr::editor::panel::keyframes::title())
                    .value(controls.into_any_element()),
            )
            .child(
                div()
                    .text_size(px(12.))
                    .line_height(px(17.))
                    .text_color(Hsla::from(theme.editor.text_3))
                    .child(if animated {
                        tr::editor::panel::keyframes::hint_active()
                    } else {
                        tr::editor::panel::keyframes::hint_empty()
                    }),
            )
            .when_some(span.zip(interpolation), |column, (key, current)| {
                column.child(
                    ui::Field::inline(&theme, tr::editor::panel::keyframes::transition()).child(
                        ui::SegmentedControl::editor(
                            &theme,
                            SharedString::from(format!("image-keyframe-motion-{index}")),
                            vec![
                                ui::SegmentOption::new(
                                    tr::editor::panel::keyframes::smooth(),
                                    current == KeyframeInterpolation::Smooth,
                                ),
                                ui::SegmentOption::new(
                                    tr::editor::panel::keyframes::linear(),
                                    current == KeyframeInterpolation::Linear,
                                ),
                                ui::SegmentOption::new(
                                    tr::editor::panel::keyframes::hold(),
                                    current == KeyframeInterpolation::Hold,
                                ),
                            ],
                        )
                        .on_select(cx.listener(
                            move |this, choice: &usize, window, cx| {
                                let Some(value) = INTERPOLATIONS.get(*choice).copied() else {
                                    return;
                                };
                                this.edit_image_segment(
                                    "image-keyframe-motion",
                                    index,
                                    window,
                                    cx,
                                    move |segment| segment.set_keyframe_interpolation(key, value),
                                );
                            },
                        )),
                    ),
                )
            })
            .when(animated, |column| {
                column.child(
                    ui::EditorButton::plain(
                        &theme,
                        SharedString::from(format!("image-keyframe-clear-{index}")),
                    )
                    .tinted(&theme)
                    .height(px(30.))
                    .left_icon("icons/trash.svg")
                    .icon_size(px(13.))
                    .label(tr::editor::panel::keyframes::clear())
                    .on_click(cx.listener(move |this, _, window, cx| {
                        let time = this.preview_or_playhead();
                        this.edit_image_segment(
                            "image-keyframes-clear",
                            index,
                            window,
                            cx,
                            move |segment| segment.clear_keyframes(time),
                        );
                    })),
                )
            })
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
}
