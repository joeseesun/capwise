use std::ops::Range;

use cap_project::{CaptionRecognition, CaptionSegment, CaptionTrackSegment};
use gpui::{
    AnyElement, Context, FontWeight, Hsla, InteractiveElement, IntoElement, ParentElement,
    SharedString, StatefulInteractiveElement, Styled, Window, div, px, svg,
};
use serde_json::Value;

use crate::{
    caption_review,
    editor_tabs::{self, SidebarMenu},
    editor_window::EditorWindow,
    editor_workbench::workbench_time,
    i18n::tr,
    transcription::{self, regenerate},
    ui,
};

#[derive(Default)]
pub(crate) struct RegenerationUi {
    open: bool,
    chosen: Option<CaptionRecognition>,
    full: bool,
    suspect_count: usize,
    running: bool,
    error: Option<String>,
    draft: Option<Draft>,
    audition: Option<usize>,
}

struct Draft {
    source: CaptionRecognition,
    segments: Vec<CaptionSegment>,
    preview: Vec<CaptionTrackSegment>,
    ranges: Vec<Range<f64>>,
    full: bool,
    baseline: Value,
}

impl EditorWindow {
    fn caption_regeneration_baseline(&self) -> Value {
        serde_json::json!([
            caption_review::caption_content_snapshot(&self.project),
            self.project
                .timeline
                .as_ref()
                .map(|timeline| &timeline.segments),
            &self.clip_display_durations,
        ])
    }

    fn caption_regeneration_models(&self) -> Vec<(CaptionRecognition, String)> {
        let models = editor_tabs::available_caption_models();
        let previous = self
            .project
            .caption_recognition
            .as_ref()
            .filter(|previous| {
                models.iter().any(|model| model.name == previous.engine)
                    && regenerate::recognition(&previous.engine, &previous.language) == **previous
            });
        previous
            .map(|source| {
                (
                    source.clone(),
                    tr::editor::regenerate::previous_model(caption_model_label(source)),
                )
            })
            .into_iter()
            .chain(models.into_iter().filter_map(|model| {
                let source = regenerate::recognition(model.name, self.sidebar.caption_language);
                (Some(&source) != previous).then(|| {
                    let label = caption_model_label(&source);
                    (source, label)
                })
            }))
            .collect()
    }

    fn caption_regeneration_unconfigured_services(
        ready: &[(CaptionRecognition, String)],
    ) -> impl Iterator<Item = &'static crate::asr_cloud::AsrService> + '_ {
        crate::asr_cloud::SERVICES
            .iter()
            .filter(|service| !ready.iter().any(|(source, _)| source.engine == service.id))
    }

    pub(crate) fn caption_regeneration_model_items(&self) -> Vec<ui::MenuItem> {
        let current = self.sidebar.caption_regeneration.chosen.as_ref();
        let ready = self.caption_regeneration_models();
        ready
            .iter()
            .map(|(source, label)| ui::MenuItem::new(label.clone(), current == Some(source)))
            .chain(
                Self::caption_regeneration_unconfigured_services(&ready).map(|service| {
                    ui::MenuItem::new(
                        tr::editor::regenerate::configure_model((service.name)()),
                        false,
                    )
                }),
            )
            .collect()
    }

    pub(crate) fn choose_caption_regeneration_model(
        &mut self,
        index: usize,
        cx: &mut Context<Self>,
    ) {
        let mut ready = self.caption_regeneration_models();
        if index < ready.len() {
            let (source, _) = ready.swap_remove(index);
            self.sidebar.caption_regeneration.chosen = Some(source);
            self.sidebar.caption_regeneration.error = None;
        } else if let Some(service) =
            Self::caption_regeneration_unconfigured_services(&ready).nth(index - ready.len())
        {
            let id = service.id;
            cx.defer(move |cx| crate::app_windows::open_asr_provider_settings(id, cx));
        }
        self.invalidate_sidebar(cx);
    }

    fn open_caption_regeneration(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let state = &mut self.sidebar.caption_regeneration;
        if state.running || state.draft.is_some() || state.error.is_some() {
            state.open = true;
            self.invalidate_sidebar(cx);
            return;
        }
        self.commit_caption_drafts(window, cx);
        self.end_field_edit(cx);
        let ranges = cap_project::transcription_source_ranges(
            self.project.timeline.as_ref(),
            &self.clip_display_durations,
        );
        let suspect_count = regenerate::suspicious_ranges(&self.project, &ranges).len();
        let preferred = regenerate::recognition(
            self.selected_caption_model().name,
            self.sidebar.caption_language,
        );
        let models = self.caption_regeneration_models();
        let state = &self.sidebar.caption_regeneration;
        let chosen = state
            .chosen
            .as_ref()
            .or(self.project.caption_recognition.as_ref())
            .and_then(|previous| models.iter().find(|(source, _)| source == previous))
            .map(|(source, _)| source.clone())
            .unwrap_or(preferred);
        self.sidebar.caption_regeneration = RegenerationUi {
            open: true,
            chosen: Some(chosen),
            full: suspect_count == 0 || state.chosen.is_some() && state.full,
            suspect_count,
            ..Default::default()
        };
        self.invalidate_sidebar(cx);
    }

    fn close_caption_regeneration(&mut self, cx: &mut Context<Self>) {
        if self.sidebar.caption_regeneration.audition.is_some() {
            self.stop_playback(cx);
        }
        if self
            .sidebar
            .menu
            .as_ref()
            .is_some_and(|menu| menu.kind == SidebarMenu::CaptionRegenerationModel)
        {
            self.sidebar.menu = None;
        }
        self.sidebar.caption_regeneration.open = false;
        self.invalidate_sidebar(cx);
    }

    pub(crate) fn clear_caption_regeneration_audition(&mut self, cx: &mut Context<Self>) {
        if self.sidebar.caption_regeneration.audition.take().is_some() {
            self.invalidate_sidebar(cx);
        }
    }

    fn begin_caption_regeneration(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.project_ready() || self.voiceover.is_busy() || self.asset_import_busy() {
            return;
        }
        self.commit_caption_drafts(window, cx);
        self.end_field_edit(cx);
        let state = &mut self.sidebar.caption_regeneration;
        let Some(source) = state.chosen.clone() else {
            return;
        };
        if state.running {
            return;
        }
        let full = state.full;
        let visible = cap_project::transcription_source_ranges(
            self.project.timeline.as_ref(),
            &self.clip_display_durations,
        );
        let ranges = if full {
            visible
        } else {
            regenerate::suspicious_ranges(&self.project, &visible)
        };
        if ranges.is_empty() {
            if !full {
                state.suspect_count = 0;
                state.full = true;
                self.invalidate_sidebar(cx);
            }
            return;
        }
        if !transcription::begin_generation(&self.project_path) {
            return;
        }
        state.running = true;
        state.error = None;
        state.draft = None;
        let baseline = self.caption_regeneration_baseline();
        let project = self.project_path.clone();
        let durations = self.clip_display_durations.clone();
        let timeline = self.project.timeline.clone();
        self.invalidate_sidebar(cx);
        cx.spawn_in(window, async move |this, cx| {
            let task_project = project.clone();
            let result = gpui_tokio::Tokio::spawn(cx, async move {
                tokio::task::spawn_blocking(move || {
                    let segments =
                        regenerate::generate(&task_project, &source, &ranges, &durations)?;
                    let preview = timeline
                        .as_ref()
                        .map(|timeline| {
                            transcription::derive_caption_track_segments(
                                &segments, timeline, &durations,
                            )
                        })
                        .unwrap_or_default();
                    Ok(Draft {
                        source,
                        segments,
                        preview,
                        ranges,
                        full,
                        baseline,
                    })
                })
                .await
                .map_err(|error| error.to_string())?
            })
            .await
            .unwrap_or_else(|error| Err(error.to_string()));
            transcription::finish_generation(&project, None);
            let _ = this.update_in(cx, |this, _, cx| {
                let state = &mut this.sidebar.caption_regeneration;
                state.running = false;
                match result {
                    Ok(draft) => state.draft = Some(draft),
                    Err(error) => {
                        state.error = Some(transcription::caption_generation_error_message(&error))
                    }
                }
                this.invalidate_sidebar(cx);
            });
        })
        .detach();
    }

    fn apply_caption_regeneration(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.project_ready()
            || self.voiceover.is_busy()
            || self.asset_import_busy()
            || transcription::ui_snapshot(&self.project_path).generating
        {
            return;
        }
        self.commit_caption_drafts(window, cx);
        self.finish_caption_edit(window, cx);
        self.end_field_edit(cx);
        if self.sidebar.caption_regeneration.audition.is_some() {
            self.stop_playback(cx);
        }
        let Some(draft) = &self.sidebar.caption_regeneration.draft else {
            return;
        };
        if self.caption_regeneration_baseline() != draft.baseline {
            self.sidebar.caption_regeneration.error =
                Some(tr::editor::review::changed().to_string());
            self.invalidate_sidebar(cx);
            return;
        }
        let archive = self.project_path.join("caption-reviews");
        let backup = std::fs::create_dir_all(&archive)
            .map_err(|error| error.to_string())
            .and_then(|()| {
                caption_review::write_json(
                    &archive.join(format!("before-regeneration-{}.json", uuid::Uuid::new_v4())),
                    &serde_json::json!({
                        "captions": self.project.captions,
                        "captionSegments": self.project.timeline.as_ref().map(|timeline| &timeline.caption_segments),
                        "captionRecognition": self.project.caption_recognition,
                    }),
                )
            });
        if let Err(error) = backup {
            self.sidebar.caption_regeneration.error = Some(error);
            self.invalidate_sidebar(cx);
            return;
        }
        let Some(draft) = std::mem::take(&mut self.sidebar.caption_regeneration).draft else {
            return;
        };
        let durations = self.clip_display_durations.clone();
        self.cancel_caption_processing(cx);
        self.edit_project_global("caption-regeneration", window, cx, move |project| {
            regenerate::apply(
                project,
                draft.source,
                draft.segments,
                &draft.ranges,
                draft.full,
                &durations,
            );
            true
        });
        self.invalidate_sidebar(cx);
    }

    pub(crate) fn render_caption_regeneration(
        &self,
        has_audio: bool,
        generating: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let state = &self.sidebar.caption_regeneration;
        let theme = &self.theme;
        if !state.open {
            let label = if state.running {
                tr::editor::regenerate::view_progress()
            } else if state.draft.is_some() {
                tr::editor::regenerate::view_preview()
            } else {
                tr::editor::regenerate::open()
            };
            return ui::EditorButton::plain(theme, "caption-regenerate")
                .tinted(theme)
                .start()
                .grow()
                .height(px(32.))
                .left_icon("icons/refresh-cw.svg")
                .right_icon("icons/chevron-right.svg")
                .right_icon_end(true)
                .label(label)
                .disabled((!has_audio || generating) && !state.running && state.draft.is_none())
                .on_click(
                    cx.listener(|this, _, window, cx| this.open_caption_regeneration(window, cx)),
                )
                .into_any_element();
        }
        let label = if state.draft.is_some() {
            tr::editor::regenerate::preview()
        } else {
            tr::editor::regenerate::open()
        };
        let heading = div()
            .flex()
            .items_center()
            .justify_between()
            .child(
                div()
                    .text_size(px(14.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(label),
            )
            .child(
                ui::EditorButton::plain(theme, "close-caption-regeneration")
                    .width(px(28.))
                    .left_icon("icons/x.svg")
                    .icon_size(px(14.))
                    .tooltip(theme, tr::common::close())
                    .on_click(cx.listener(|this, _, _, cx| this.close_caption_regeneration(cx))),
            );
        let secondary = Hsla::from(theme.editor.text_2);
        let hint = |text: SharedString| div().text_size(px(12.)).text_color(secondary).child(text);
        let panel = div()
            .flex()
            .flex_col()
            .gap(px(10.))
            .pb(px(12.))
            .border_b_1()
            .border_color(Hsla::from(theme.editor.line))
            .child(heading);
        if state.running {
            return panel
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .py(px(5.))
                        .child(
                            ui::CircularProgress::new(
                                px(16.),
                                px(2.),
                                Hsla::from(theme.editor.ctl_active),
                                Hsla::from(theme.editor.accent),
                            )
                            .indeterminate(),
                        )
                        .child(hint(tr::editor::regenerate::validating().into())),
                )
                .into_any_element();
        }
        let body = match &state.draft {
            Some(draft) => self.render_caption_regeneration_preview(draft, generating, cx),
            None => self.render_caption_regeneration_setup(has_audio, generating, cx),
        };
        panel
            .children(state.error.as_ref().map(|error| {
                div()
                    .text_size(px(12.))
                    .text_color(Hsla::from(theme.editor.text_1))
                    .border_l_2()
                    .border_color(Hsla::from(theme.editor.accent))
                    .pl(px(8.))
                    .child(error.clone())
            }))
            .child(body)
            .into_any_element()
    }

    fn render_caption_regeneration_setup(
        &self,
        has_audio: bool,
        generating: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let state = &self.sidebar.caption_regeneration;
        let theme = &self.theme;
        let chosen = state.chosen.as_ref();
        let label = chosen.map(caption_model_label).unwrap_or_default();
        let model = ui::Field::inline(theme, tr::editor::regenerate::model_label()).child(
            div().w(px(188.)).max_w_full().child(self.menu_select(
                SidebarMenu::CaptionRegenerationModel,
                "caption-regeneration-model",
                label,
                cx,
            )),
        );
        let details = if chosen.is_some_and(|model| {
            crate::asr_cloud::is_cloud_model(&model.engine) && model.engine != "local-server"
        }) {
            Some(tr::editor::regenerate::cloud())
        } else if chosen.is_some() && chosen == self.project.caption_recognition.as_ref() {
            Some(tr::editor::regenerate::using_previous())
        } else {
            None
        };
        let row = if state.suspect_count == 0 {
            ui::Field::inline(theme, tr::editor::regenerate::scope_label())
                .value_text(tr::editor::regenerate::full())
                .into_any_element()
        } else {
            ui::Field::inline(theme, tr::editor::regenerate::scope_label())
                .child(
                    div().w(px(188.)).max_w_full().child(
                        ui::SegmentedControl::editor(
                            theme,
                            "caption-regeneration-scope",
                            vec![
                                ui::SegmentOption::new(
                                    tr::editor::regenerate::suspect(),
                                    !state.full,
                                ),
                                ui::SegmentOption::new(tr::editor::regenerate::full(), state.full),
                            ],
                        )
                        .stretch()
                        .on_select(cx.listener(
                            |this, index: &usize, _, cx| {
                                this.sidebar.caption_regeneration.full = *index == 1;
                                this.sidebar.caption_regeneration.error = None;
                                this.invalidate_sidebar(cx);
                            },
                        )),
                    ),
                )
                .into_any_element()
        };
        let range_details: SharedString = if state.suspect_count == 0 {
            tr::editor::regenerate::no_anomalies().into()
        } else {
            tr::editor::regenerate::anomalies(state.suspect_count).into()
        };
        let snapshot = transcription::ui_snapshot(&self.project_path);
        let missing_model = chosen.filter(|selected| {
            !crate::asr_cloud::is_cloud_model(&selected.engine)
                && !snapshot.downloaded.contains(&selected.engine)
        });
        let primary = if let Some(model) = missing_model {
            let model = model.engine.clone();
            ui::EditorButton::plain(theme, "caption-regenerate-download")
                .filled(theme)
                .label(if transcription::download_active() {
                    tr::editor::regenerate::downloading()
                } else {
                    tr::editor::regenerate::download_model()
                })
                .disabled(transcription::download_active())
                .on_click(cx.listener(move |this, _, _, cx| {
                    if transcription::begin_download(&model) {
                        gpui_tokio::Tokio::spawn(
                            cx,
                            transcription::run_model_download(model.clone()),
                        )
                        .detach();
                        this.spawn_caption_download_poll(cx);
                        this.invalidate_sidebar(cx);
                    }
                }))
        } else {
            ui::EditorButton::plain(theme, "generate-caption-draft")
                .filled(theme)
                .label(tr::editor::regenerate::draft())
                .disabled(generating || !has_audio || chosen.is_none())
                .on_click(
                    cx.listener(|this, _, window, cx| this.begin_caption_regeneration(window, cx)),
                )
        };
        let hint = |message: SharedString| {
            div()
                .text_size(px(12.))
                .text_color(Hsla::from(theme.editor.text_2))
                .child(message)
        };
        div()
            .flex()
            .flex_col()
            .gap(px(10.))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(4.))
                    .child(model)
                    .children(details.map(|text| hint(text.into()))),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(4.))
                    .child(row)
                    .child(hint(range_details)),
            )
            .child(
                div()
                    .flex()
                    .justify_end()
                    .child(primary.height(px(32.)).padding_x(px(13.))),
            )
            .into_any_element()
    }

    fn render_caption_regeneration_preview(
        &self,
        draft: &Draft,
        generating: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let state = &self.sidebar.caption_regeneration;
        let theme = &self.theme;
        let ink = Hsla::from(theme.editor.text_1);
        let muted = Hsla::from(theme.editor.text_2);
        let accent = Hsla::from(theme.editor.accent);
        let list = div()
            .id("caption-draft-list")
            .max_h(px(192.))
            .overflow_y_scroll()
            .border_y_1()
            .border_color(Hsla::from(theme.editor.line))
            .flex()
            .flex_col()
            .children(draft.preview.iter().enumerate().map(|(index, cue)| {
                let time = workbench_time(cue.start);
                let (from, until) = (cue.start, cue.end);
                let active = state.audition == Some(index);
                div()
                    .flex()
                    .items_start()
                    .gap(px(8.))
                    .py(px(4.))
                    .child(
                        ui::EditorButton::plain(
                            theme,
                            SharedString::from(format!("draft-caption-{index}")),
                        )
                        .width(px(64.))
                        .start()
                        .left_icon(if active {
                            "icons/pause.svg"
                        } else {
                            "icons/play.svg"
                        })
                        .icon_size(px(11.))
                        .label(time.clone())
                        .tooltip(
                            theme,
                            if active {
                                tr::editor::regenerate::pause().to_owned()
                            } else {
                                tr::editor::regenerate::listen(time)
                            },
                        )
                        .pressed(active)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if this.sidebar.caption_regeneration.audition == Some(index) {
                                this.stop_playback(cx);
                            } else {
                                this.play_range(from, until, cx);
                                if this.playing {
                                    this.sidebar.caption_regeneration.audition = Some(index);
                                    this.invalidate_sidebar(cx);
                                }
                            }
                        })),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .py(px(4.))
                            .text_size(px(13.))
                            .text_color(if active { accent } else { ink })
                            .child(cue.text.clone()),
                    )
            }));
        div()
            .flex()
            .flex_col()
            .gap(px(8.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .child(
                        svg()
                            .path("icons/circle-check.svg")
                            .size(px(14.))
                            .text_color(accent),
                    )
                    .child(
                        div()
                            .text_size(px(12.))
                            .text_color(muted)
                            .child(tr::editor::regenerate::verified(draft.segments.len())),
                    ),
            )
            .child(list)
            .child(
                div()
                    .text_size(px(12.))
                    .text_color(muted)
                    .child(tr::editor::regenerate::backup()),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(px(4.))
                    .child(
                        ui::EditorButton::plain(theme, "caption-regeneration-back")
                            .left_icon("icons/chevron-left.svg")
                            .icon_size(px(12.))
                            .label(tr::editor::regenerate::back())
                            .on_click(cx.listener(|this, _, _, cx| {
                                if this.sidebar.caption_regeneration.audition.is_some() {
                                    this.stop_playback(cx);
                                }
                                let state = &mut this.sidebar.caption_regeneration;
                                state.draft = None;
                                state.error = None;
                                this.invalidate_sidebar(cx);
                            })),
                    )
                    .child(
                        ui::EditorButton::plain(theme, "apply-caption-regeneration")
                            .filled(theme)
                            .height(px(32.))
                            .padding_x(px(12.))
                            .label(tr::editor::regenerate::apply())
                            .disabled(generating)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.apply_caption_regeneration(window, cx)
                            })),
                    ),
            )
            .into_any_element()
    }
}

fn caption_model_label(source: &CaptionRecognition) -> String {
    let label = editor_tabs::caption_model_label(&source.engine);
    if source.engine == source.model {
        label
    } else {
        format!("{label} · {}", source.model)
    }
}
