//! [INPUT]: 配音状态、输入电平和编辑器主题。
//! [OUTPUT]: 工程侧栏配音控件和画面倒数。
//! [POS]: 配音交互的渲染层。
//! [PROTOCOL]: Update this header on change, then check AGENTS.md

use gpui::{
    AnyElement, Context, FontWeight, Hsla, InteractiveElement as _, IntoElement as _,
    ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _, div,
    prelude::FluentBuilder, px,
};

use crate::{
    editor_window::EditorWindow, feeds::Feeds, i18n::tr, ui, voiceover_recording::TakeAvailability,
};

use super::{Phase, clock};

impl EditorWindow {
    pub(crate) fn render_voiceover(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let panel = self.voiceover.panel.as_ref()?;
        let theme = self.theme;
        let busy = panel.phase.busy();
        let elapsed = panel
            .ready
            .as_ref()
            .map(|take| take.audio().duration)
            .unwrap_or_else(|| {
                panel
                    .status
                    .as_ref()
                    .map_or(0., |status| status.duration_estimate.as_secs_f64())
            });
        let range = panel.range(self.total_duration());
        let start = panel.ready.as_ref().map_or(
            if panel.follow_picture {
                range.start
            } else {
                panel.origin
            },
            |take| take.manifest().output_start,
        );
        let setup = matches!(panel.phase, Phase::Idle | Phase::Error);
        let recording = matches!(panel.phase, Phase::Recording | Phase::Paused);
        let ready = panel.phase == Phase::Ready;
        let attached = panel.phase == Phase::Attached;
        let has_sample = panel
            .status
            .as_ref()
            .is_some_and(|status| status.first_sample_seen);
        let awaiting_picture = panel.sync.as_ref().is_some_and(|clock| !clock.is_running());
        let phase = match panel.phase {
            Phase::Idle => tr::editor::voiceover::ready(),
            Phase::Starting if panel.countdown.is_some() => tr::editor::voiceover::get_ready(),
            Phase::Starting => tr::editor::voiceover::preparing(),
            Phase::Recording if awaiting_picture => tr::editor::voiceover::starting_picture(),
            Phase::Recording if !has_sample => tr::editor::voiceover::preparing(),
            Phase::Recording => tr::editor::voiceover::recording(),
            Phase::Paused => tr::editor::voiceover::paused(),
            Phase::Finalizing | Phase::SourceFinalizing => tr::editor::voiceover::saving(),
            Phase::Recovering => tr::editor::voiceover::recovering(),
            Phase::Ready => tr::editor::voiceover::review_title(),
            Phase::Attached => tr::editor::voiceover::added(),
            Phase::Error => tr::editor::voiceover::stopped(),
        };
        let takes = panel
            .takes
            .iter()
            .filter(|take| {
                take.availability != TakeAvailability::Attached
                    && take.manifest.as_ref().is_none_or(|manifest| {
                        Some(manifest.id) != panel.ready.as_ref().map(|take| take.manifest().id)
                    })
            })
            .collect::<Vec<_>>();
        let close_disabled = matches!(
            panel.phase,
            Phase::Finalizing | Phase::SourceFinalizing | Phase::Recovering
        );
        let mut content = div()
            .id("voiceover-panel")
            .size_full()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .p(px(16.))
            .gap(px(12.))
            .text_color(Hsla::from(theme.gray_12))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(px(8.))
                    .child(
                        div()
                            .text_size(px(16.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(if panel.replacement.is_some() {
                                tr::editor::voiceover::replace_title()
                            } else {
                                tr::editor::voiceover::title()
                            }),
                    )
                    .child(
                        ui::EditorButton::plain(&theme, "voiceover-close")
                            .label(tr::common::close())
                            .disabled(close_disabled)
                            .on_click(
                                cx.listener(|this, _, window, cx| this.close_voiceover(window, cx)),
                            ),
                    ),
            );
        if setup && panel.replacement.is_none() {
            let mut picture = ui::SegmentOption::new(
                tr::editor::voiceover::follow_picture(),
                panel.follow_picture,
            );
            picture.icon = Some("icons/video.svg".into());
            let mut audio =
                ui::SegmentOption::new(tr::editor::voiceover::audio_only(), !panel.follow_picture);
            audio.icon = Some("icons/microphone.svg".into());
            content = content.child(
                ui::SegmentedControl::editor(&theme, "voiceover-mode", vec![picture, audio])
                    .stretch()
                    .text_size(px(13.))
                    .item_height(px(32.))
                    .on_select(cx.listener(|this, index: &usize, _, cx| {
                        if let Some(panel) = this.voiceover.panel.as_mut() {
                            panel.follow_picture = *index == 0;
                            panel.error = None;
                        }
                        cx.notify();
                    })),
            );
        }
        if setup
            && panel.follow_picture
            && panel.selected_range.is_some()
            && panel.replacement.is_none()
        {
            content = content.child(
                ui::SegmentedControl::pills(
                    &theme,
                    "voiceover-range",
                    vec![
                        ui::SegmentOption::new(
                            tr::editor::voiceover::from_playhead(),
                            !panel.use_range,
                        ),
                        ui::SegmentOption::new(
                            tr::editor::voiceover::selected_clip(),
                            panel.use_range,
                        ),
                    ],
                )
                .item_height(px(28.))
                .on_select(cx.listener(|this, index: &usize, _, cx| {
                    if let Some(panel) = this.voiceover.panel.as_mut() {
                        panel.use_range = *index == 1;
                        panel.error = None;
                    }
                    cx.notify();
                })),
            );
        }
        let mut timing = div()
            .flex()
            .items_center()
            .justify_between()
            .gap(px(8.))
            .text_size(px(12.))
            .text_color(Hsla::from(theme.gray_11))
            .child(if setup && panel.follow_picture {
                format!("{} – {}", clock(start), clock(range.end))
            } else {
                clock(start)
            });
        if panel.follow_picture && !ready && !attached {
            timing = timing.child(div().child(tr::editor::voiceover::muted_picture()));
        }
        content = content.child(timing);
        if setup {
            content = content.child(
                ui::Select::plain(
                    &theme,
                    "voiceover-microphone",
                    panel
                        .selected
                        .clone()
                        .unwrap_or_else(|| tr::editor::voiceover::choose_microphone().into()),
                )
                .stretch_label()
                .on_click(cx.listener(|this, _, _, cx| {
                    if let Some(panel) = this.voiceover.panel.as_mut() {
                        panel.device_menu = !panel.device_menu;
                    }
                    cx.notify();
                })),
            );
            if panel.device_menu {
                content = content.child(
                    div()
                        .id("voiceover-devices")
                        .max_h(px(126.))
                        .overflow_y_scroll()
                        .flex()
                        .flex_col()
                        .children(panel.devices.iter().enumerate().map(|(index, name)| {
                            let name = name.clone();
                            ui::EditorButton::plain(
                                &theme,
                                SharedString::from(format!("voiceover-mic-{index}")),
                            )
                            .label(name.clone())
                            .pressed(panel.selected.as_ref() == Some(&name))
                            .on_click(cx.listener(
                                move |this, _, _, cx| {
                                    if let Some(panel) = this.voiceover.panel.as_mut() {
                                        panel.selected = Some(name.clone());
                                        panel.device_menu = false;
                                        panel.error = None;
                                    }
                                    Feeds::global(cx).update(cx, |feeds, cx| {
                                        feeds.set_microphone(Some(name.clone()), cx)
                                    });
                                    cx.notify();
                                },
                            ))
                        })),
                );
            }
        }
        if setup || recording {
            content = content.child(panel.meter.clone());
        }
        if !setup {
            let running = panel.phase == Phase::Recording && has_sample && !awaiting_picture;
            content = content.child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(px(8.))
                    .px(px(12.))
                    .py(px(10.))
                    .rounded(px(9.))
                    .bg(Hsla::from(theme.gray_3))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .when(running, |status| {
                                status.child(
                                    div()
                                        .size(px(7.))
                                        .rounded_full()
                                        .bg(Hsla::from(theme.red_9)),
                                )
                            })
                            .child(
                                div()
                                    .text_size(px(13.))
                                    .font_weight(FontWeight::MEDIUM)
                                    .child(phase),
                            ),
                    )
                    .child(
                        div()
                            .text_size(px(20.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(
                                panel
                                    .countdown
                                    .map_or_else(|| clock(elapsed), |count| count.to_string()),
                            ),
                    ),
            );
        }
        if ready
            && panel
                .ready
                .as_ref()
                .and_then(|take| take.audio().suggested_gain_db())
                .is_some()
        {
            content = content.child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(px(8.))
                    .text_size(px(13.))
                    .child(tr::editor::voiceover::normalize())
                    .child(
                        ui::Toggle::plain(&theme, "voiceover-normalize", panel.normalize).on_click(
                            cx.listener(|this, _, _, cx| {
                                this.stop_playback(cx);
                                this.library_audition = None;
                                if let Some(panel) = this.voiceover.panel.as_mut() {
                                    panel.normalize = !panel.normalize;
                                }
                                cx.notify();
                            }),
                        ),
                    ),
            );
        }
        if let Some(error) = &panel.error {
            content = content.child(
                div()
                    .text_size(px(12.))
                    .text_color(Hsla::from(theme.red_9))
                    .child(error.clone()),
            );
        }
        if self.audio_library.audition_failed {
            content = content.child(
                div()
                    .text_size(px(12.))
                    .child(tr::editor::audio::library::audition_failed()),
            );
        }
        if setup {
            content = content.child(
                ui::EditorButton::plain(&theme, "voiceover-start")
                    .filled(&theme)
                    .height(px(38.))
                    .left_icon("icons/microphone.svg")
                    .label(if panel.follow_picture {
                        tr::editor::voiceover::start_synced()
                    } else {
                        tr::editor::voiceover::start()
                    })
                    .disabled(panel.selected.is_none())
                    .on_click(cx.listener(|this, _, window, cx| this.start_voiceover(window, cx))),
            );
            #[cfg(target_os = "macos")]
            if panel.selected.is_none() || panel.error.is_some() {
                content = content.child(
                    ui::EditorButton::plain(&theme, "voiceover-input-settings")
                        .text(&theme)
                        .label(tr::editor::voiceover::input_settings())
                        .on_click(|_, _, cx| {
                            cx.open_url(
                                "x-apple.systempreferences:com.apple.Sound-Settings.extension",
                            )
                        }),
                );
            }
        } else if recording {
            let paused = panel.phase == Phase::Paused;
            content = content.child(
                div()
                    .flex()
                    .gap(px(8.))
                    .child(
                        ui::EditorButton::plain(&theme, "voiceover-pause")
                            .grow()
                            .height(px(36.))
                            .left_icon(if paused {
                                "icons/play.svg"
                            } else {
                                "icons/pause.svg"
                            })
                            .label(if paused {
                                tr::editor::voiceover::resume()
                            } else {
                                tr::editor::voiceover::pause()
                            })
                            .disabled(panel.pause_pending)
                            .on_click(
                                cx.listener(|this, _, window, cx| this.pause_voiceover(window, cx)),
                            ),
                    )
                    .child(
                        ui::EditorButton::plain(&theme, "voiceover-finish")
                            .filled(&theme)
                            .grow()
                            .height(px(36.))
                            .left_icon("icons/square.svg")
                            .label(tr::editor::voiceover::finish_take())
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.finish_voiceover(false, window, cx)
                            })),
                    ),
            );
        } else if ready || attached {
            let picture = panel.follow_picture || panel.replacement.is_some() || attached;
            let auditioning = self
                .library_audition
                .as_ref()
                .is_some_and(|audition| audition.id == "voiceover-take");
            let playing = self.playing || auditioning;
            content = content.child(
                div()
                    .flex()
                    .gap(px(8.))
                    .child(
                        ui::EditorButton::plain(&theme, "voiceover-review")
                            .grow()
                            .height(px(34.))
                            .left_icon(if playing {
                                "icons/pause.svg"
                            } else {
                                "icons/play.svg"
                            })
                            .label(if playing {
                                tr::editor::voiceover::stop_playback()
                            } else if picture {
                                tr::editor::voiceover::play_picture()
                            } else {
                                tr::editor::voiceover::audition_take()
                            })
                            .on_click(cx.listener(|this, _, _, cx| this.play_voiceover_take(cx))),
                    )
                    .when(ready, |row| {
                        row.child(
                            ui::EditorButton::plain(&theme, "voiceover-retake")
                                .grow()
                                .height(px(34.))
                                .left_icon("icons/rotate-ccw.svg")
                                .label(tr::editor::voiceover::retake())
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.retake_voiceover(window, cx)
                                })),
                        )
                    }),
            );
            if ready {
                content = content.child(
                    ui::EditorButton::plain(&theme, "voiceover-attach")
                        .filled(&theme)
                        .height(px(38.))
                        .left_icon("icons/check.svg")
                        .label(if panel.replacement.is_some() {
                            tr::editor::voiceover::replace_take()
                        } else {
                            tr::editor::voiceover::add_to_timeline()
                        })
                        .on_click(
                            cx.listener(|this, _, window, cx| this.attach_voiceover(window, cx)),
                        ),
                );
            }
        }
        if !takes.is_empty() && !busy {
            content = content.child(
                ui::EditorButton::plain(&theme, "voiceover-recovery-toggle")
                    .text(&theme)
                    .label(tr::editor::voiceover::recover_count(takes.len()))
                    .on_click(cx.listener(|this, _, _, cx| {
                        if let Some(panel) = this.voiceover.panel.as_mut() {
                            panel.recovery_open = !panel.recovery_open;
                        }
                        cx.notify();
                    })),
            );
            if panel.recovery_open {
                content = content.child(div().flex().flex_col().gap(px(8.)).children(
                    takes.into_iter().enumerate().map(|(index, take)| {
                        let label = take.manifest.as_ref().map_or_else(
                            || tr::editor::voiceover::unknown_take().into(),
                            |manifest| {
                                tr::editor::voiceover::asset_name(clock(manifest.output_start))
                            },
                        );
                        let id = take.manifest.as_ref().map(|manifest| manifest.id);
                        let recoverable = matches!(
                            take.availability,
                            TakeAvailability::Ready | TakeAvailability::Interrupted
                        );
                        let state = match take.availability {
                            TakeAvailability::Active => tr::editor::voiceover::take_active(),
                            TakeAvailability::Missing => tr::editor::voiceover::take_missing(),
                            TakeAvailability::Empty => tr::editor::voiceover::take_empty(),
                            TakeAvailability::Invalid => tr::editor::voiceover::take_invalid(),
                            TakeAvailability::Cancelled => tr::editor::voiceover::take_cancelled(),
                            _ => tr::editor::voiceover::recover(),
                        };
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .gap(px(8.))
                            .child(div().text_size(px(12.)).child(label))
                            .child(
                                ui::EditorButton::plain(
                                    &theme,
                                    SharedString::from(format!("voiceover-recover-{index}")),
                                )
                                .label(state)
                                .disabled(!recoverable)
                                .on_click(cx.listener(
                                    move |this, _, window, cx| {
                                        if let Some(id) = id {
                                            this.recover_voiceover(id, window, cx);
                                        }
                                    },
                                )),
                            )
                    }),
                ));
            }
        }
        Some(content.into_any_element())
    }

    pub(crate) fn render_voiceover_countdown(&self) -> Option<AnyElement> {
        let count = self.voiceover.panel.as_ref()?.countdown?;
        Some(
            div()
                .absolute()
                .inset_0()
                .flex()
                .items_center()
                .justify_center()
                .child(
                    div()
                        .size(px(68.))
                        .rounded_full()
                        .flex()
                        .items_center()
                        .justify_center()
                        .bg(gpui::black().opacity(0.72))
                        .text_color(gpui::white())
                        .text_size(px(36.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(count.to_string()),
                )
                .into_any_element(),
        )
    }
}
