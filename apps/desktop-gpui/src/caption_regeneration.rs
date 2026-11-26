use std::ops::Range;

use cap_project::{CaptionRecognition, CaptionSegment};
use gpui::{
    AnyElement, Context, Hsla, InteractiveElement, IntoElement, ParentElement, SharedString,
    StatefulInteractiveElement, Styled, Window, div, px,
};
use serde_json::Value;

use crate::{
    caption_review, editor_tabs,
    editor_window::EditorWindow,
    i18n::tr,
    transcription::{self, regenerate},
    ui,
};

#[derive(Default)]
pub(crate) struct RegenerationUi {
    open: bool,
    model_list: bool,
    chosen: Option<CaptionRecognition>,
    full: bool,
    suspect: Vec<Range<f64>>,
    running: bool,
    error: Option<String>,
    draft: Option<Draft>,
}

struct Draft {
    source: CaptionRecognition,
    segments: Vec<CaptionSegment>,
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

    fn open_caption_regeneration(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.commit_caption_drafts(window, cx);
        self.end_field_edit(cx);
        let ranges = cap_project::transcription_source_ranges(
            self.project.timeline.as_ref(),
            &self.clip_display_durations,
        );
        let suspect = regenerate::suspicious_ranges(&self.project, &ranges);
        let chosen = self
            .project
            .caption_recognition
            .clone()
            .filter(|previous| {
                editor_tabs::available_caption_models()
                    .iter()
                    .any(|model| model.name == previous.engine)
            })
            .filter(|previous| {
                regenerate::recognition(&previous.engine, &previous.language) == *previous
            })
            .unwrap_or_else(|| {
                regenerate::recognition(
                    self.selected_caption_model().name,
                    self.sidebar.caption_language,
                )
            });
        self.sidebar.caption_regeneration = RegenerationUi {
            open: true,
            chosen: Some(chosen),
            full: suspect.is_empty(),
            suspect,
            ..Default::default()
        };
        self.invalidate_sidebar(cx);
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
        if ranges.is_empty() || !transcription::begin_generation(&self.project_path) {
            return;
        }
        state.running = true;
        state.error = None;
        state.draft = None;
        let baseline = self.caption_regeneration_baseline();
        let project = self.project_path.clone();
        let durations = self.clip_display_durations.clone();
        self.invalidate_sidebar(cx);
        cx.spawn_in(window, async move |this, cx| {
            let task_project = project.clone();
            let result = gpui_tokio::Tokio::spawn(cx, async move {
                tokio::task::spawn_blocking(move || {
                    let segments =
                        regenerate::generate(&task_project, &source, &ranges, &durations)?;
                    Ok(Draft {
                        source,
                        segments,
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
        let Some(draft) = self.sidebar.caption_regeneration.draft.take() else {
            return;
        };
        let result = (|| {
            if self.caption_regeneration_baseline() != draft.baseline {
                return Err(tr::editor::review::changed().to_string());
            }
            let archive = self.project_path.join("caption-reviews");
            std::fs::create_dir_all(&archive).map_err(|error| error.to_string())?;
            caption_review::write_json(
                &archive.join(format!("before-regeneration-{}.json", uuid::Uuid::new_v4())),
                &serde_json::json!({
                    "captions": self.project.captions,
                    "captionSegments": self.project.timeline.as_ref().map(|timeline| &timeline.caption_segments),
                    "captionRecognition": self.project.caption_recognition,
                }),
            )?;
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
            Ok(())
        })();
        if let Err(error) = result {
            self.sidebar.caption_regeneration.error = Some(error);
        } else {
            self.sidebar.caption_regeneration = RegenerationUi::default();
        }
        self.invalidate_sidebar(cx);
    }

    pub(crate) fn render_caption_regeneration(
        &self,
        has_audio: bool,
        generating: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let state = &self.sidebar.caption_regeneration;
        let theme = self.theme;
        let mut panel = div().flex().flex_col().gap(px(8.));
        if !state.open {
            return panel
                .child(
                    ui::EditorButton::plain(&theme, "caption-regenerate")
                        .tinted(&theme)
                        .left_icon("icons/refresh-cw.svg")
                        .label(tr::editor::regenerate::open())
                        .disabled(!has_audio || generating)
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.open_caption_regeneration(window, cx)
                        })),
                )
                .into_any_element();
        }
        panel = panel
            .p(px(10.))
            .rounded(px(8.))
            .bg(Hsla::from(theme.editor.ctl));
        if state.running {
            return panel
                .child(tr::editor::regenerate::validating())
                .into_any_element();
        }
        if let Some(error) = &state.error {
            panel = panel.child(div().text_size(px(12.)).child(error.clone()));
        }
        if let Some(draft) = &state.draft {
            let projected = self
                .project
                .timeline
                .as_ref()
                .map(|timeline| {
                    transcription::derive_caption_track_segments(
                        &draft.segments,
                        timeline,
                        &self.clip_display_durations,
                    )
                })
                .unwrap_or_default();
            panel = panel
                .child(tr::editor::regenerate::verified(draft.segments.len()))
                .child(
                    div()
                        .id("caption-draft-list")
                        .max_h(px(215.))
                        .overflow_y_scroll()
                        .flex()
                        .flex_col()
                        .children(projected.into_iter().enumerate().map(|(i, cue)| {
                            ui::EditorButton::plain(
                                &theme,
                                SharedString::from(format!("draft-caption-{i}")),
                            )
                            .start()
                            .label(format!("{:.1}  {}", cue.start, cue.text))
                            .tooltip(&theme, tr::editor::regenerate::listen())
                            .on_click(cx.listener(
                                move |this, _, _, cx| this.play_range(cue.start, cue.end, cx),
                            ))
                        })),
                )
                .child(
                    ui::EditorButton::plain(&theme, "apply-caption-regeneration")
                        .tinted(&theme)
                        .label(tr::editor::regenerate::apply())
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.apply_caption_regeneration(window, cx)
                        })),
                );
        } else {
            let previous = self.project.caption_recognition.as_ref();
            let chosen = state.chosen.as_ref();
            if let Some(chosen) = chosen {
                let label = editor_tabs::caption_model_label(&chosen.engine);
                let label = if chosen.engine != chosen.model {
                    format!("{label} · {}", chosen.model)
                } else {
                    label
                };
                panel = panel.child(div().text_size(px(12.)).child(label));
                if crate::asr_cloud::is_cloud_model(&chosen.engine)
                    && chosen.engine != "local-server"
                {
                    panel = panel.child(
                        div()
                            .text_size(px(11.))
                            .child(tr::editor::regenerate::cloud()),
                    );
                }
            }
            if let Some(previous) = previous
                && editor_tabs::available_caption_models()
                    .iter()
                    .any(|model| model.name == previous.engine)
                && regenerate::recognition(&previous.engine, &previous.language) == *previous
            {
                let previous = previous.clone();
                panel = panel.child(
                    ui::EditorButton::plain(&theme, "caption-model-previous")
                        .pressed(chosen == Some(&previous))
                        .label(tr::editor::regenerate::previous())
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.sidebar.caption_regeneration.chosen = Some(previous.clone());
                            this.sidebar.caption_regeneration.model_list = false;
                            this.invalidate_sidebar(cx);
                        })),
                );
            } else {
                panel = panel.child(
                    div()
                        .text_size(px(11.))
                        .child(tr::editor::regenerate::unknown()),
                );
            }
            panel = panel.child(
                ui::EditorButton::plain(&theme, "caption-model-change")
                    .right_icon("icons/chevron-down.svg")
                    .label(tr::editor::regenerate::change_model())
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.sidebar.caption_regeneration.model_list =
                            !this.sidebar.caption_regeneration.model_list;
                        this.invalidate_sidebar(cx);
                    })),
            );
            if state.model_list {
                panel = panel.child(
                    div()
                        .id("caption-regen-models")
                        .max_h(px(166.))
                        .overflow_y_scroll()
                        .flex()
                        .flex_col()
                        .children(editor_tabs::available_caption_models().into_iter().map(
                            |model| {
                                ui::EditorButton::plain(
                                    &theme,
                                    SharedString::from(format!("regen-model-{}", model.name)),
                                )
                                .start()
                                .label((model.label)())
                                .on_click(cx.listener(
                                    move |this, _, _, cx| {
                                        this.sidebar.caption_regeneration.chosen =
                                            Some(regenerate::recognition(
                                                model.name,
                                                this.sidebar.caption_language,
                                            ));
                                        this.sidebar.caption_regeneration.model_list = false;
                                        this.sidebar.caption_regeneration.error = None;
                                        this.invalidate_sidebar(cx);
                                    },
                                ))
                            },
                        )),
                );
            }
            panel = panel.child(
                ui::SegmentedControl::editor(
                    &theme,
                    "caption-regeneration-scope",
                    vec![
                        ui::SegmentOption::new(tr::editor::regenerate::suspect(), !state.full)
                            .disabled(state.suspect.is_empty()),
                        ui::SegmentOption::new(tr::editor::regenerate::full(), state.full),
                    ],
                )
                .stretch()
                .on_select(cx.listener(|this, index: &usize, _, cx| {
                    this.sidebar.caption_regeneration.full = *index == 1;
                    this.invalidate_sidebar(cx);
                })),
            );
            let chosen_local =
                chosen.filter(|selected| !crate::asr_cloud::is_cloud_model(&selected.engine));
            let snapshot = transcription::ui_snapshot(&self.project_path);
            let download = chosen_local
                .filter(|model| !snapshot.downloaded.contains(&model.engine))
                .map(|model| model.engine.as_str());
            if let Some(model) = download {
                let model = model.to_string();
                panel = panel.child(
                    ui::EditorButton::plain(&theme, "caption-regenerate-download")
                        .label(tr::editor::tabs::captions::download(
                            editor_tabs::caption_model_label(&model),
                        ))
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
                        })),
                );
            } else {
                panel = panel.child(
                    ui::EditorButton::plain(&theme, "generate-caption-draft")
                        .tinted(&theme)
                        .disabled(generating)
                        .label(tr::editor::regenerate::draft())
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.begin_caption_regeneration(window, cx)
                        })),
                );
            }
        }
        panel
            .child(
                ui::EditorButton::plain(&theme, "discard-caption-draft")
                    .label(tr::common::cancel())
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.sidebar.caption_regeneration = RegenerationUi::default();
                        this.invalidate_sidebar(cx);
                    })),
            )
            .into_any_element()
    }
}
