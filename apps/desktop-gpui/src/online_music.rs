/*
 * [INPUT]: 读取灵录官网的配乐目录和 MP3 文件。
 * [OUTPUT]: 提供曲目搜索、试听和已校验的下载文件。
 * [POS]: 为编辑器的音频选择器提供在线配乐。
 * [PROTOCOL]: Update this header on change, then check AGENTS.md
 */
use std::{io::Write, time::Duration};

use anyhow::{Context as _, Result, ensure};
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, AppContext as _, Context, Entity, Hsla, InteractiveElement as _, IntoElement,
    ParentElement, SharedString, StatefulInteractiveElement as _, Styled as _, Window, div,
};
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::{editor_window::EditorWindow, i18n::tr, ui};

const MUSIC_ROOT: &str = "https://capwise.qiaomu.ai/music/";
const MAX_CATALOG_BYTES: usize = 2 * 1024 * 1024;
const MAX_TRACK_BYTES: usize = 32 * 1024 * 1024;
const MAX_PREVIEW_BYTES: usize = 512 * 1024;
const PAGE_SIZE: usize = 30;

#[derive(Clone, Deserialize)]
pub(crate) struct OnlineTrack {
    pub id: String,
    pub title: String,
    pub artist: String,
    pub duration: f64,
    tags: Vec<String>,
    pub preview: String,
    pub audio: String,
    #[serde(skip)]
    search_text: String,
    #[serde(skip)]
    search_title: String,
}

#[derive(Deserialize)]
struct Catalog {
    schema: u8,
    tracks: Vec<OnlineTrack>,
}

#[derive(Default)]
pub(crate) struct OnlineMusicState {
    tracks: Option<Vec<OnlineTrack>>,
    input: Option<Entity<ui::TextInputState>>,
    loading: bool,
    failed: bool,
    preview: Option<String>,
    preview_generation: u64,
    pub(crate) selected: bool,
    shown: usize,
}

impl OnlineMusicState {
    pub(crate) fn cancel_preview(&mut self) {
        self.preview = None;
        self.preview_generation = self.preview_generation.wrapping_add(1);
    }
}

fn is_hash(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

pub(crate) fn valid_track_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 80
        && id
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

async fn fetch(path: &str, limit: usize, timeout: Duration) -> Result<Vec<u8>> {
    let mut response = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(timeout)
        .build()?
        .get(format!("{MUSIC_ROOT}{path}"))
        .send()
        .await?;
    ensure!(response.status().is_success(), "music server unavailable");
    ensure!(
        response
            .content_length()
            .is_none_or(|length| length <= limit as u64),
        "music download too large"
    );
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        ensure!(
            bytes.len() + chunk.len() <= limit,
            "music download too large"
        );
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

fn read_catalog(bytes: &[u8]) -> Result<Vec<OnlineTrack>> {
    let mut catalog: Catalog = serde_json::from_slice(bytes)?;
    ensure!(catalog.schema == 1, "unsupported music catalog");
    let mut identifiers = std::collections::HashSet::new();
    for track in &mut catalog.tracks {
        ensure!(
            valid_track_id(&track.id)
                && identifiers.insert(&track.id)
                && !track.title.trim().is_empty()
                && !track.artist.trim().is_empty()
                && track.duration.is_finite()
                && track.duration > 0.0
                && is_hash(&track.preview)
                && is_hash(&track.audio),
            "invalid music catalog entry"
        );
        track.search_title = track.title.to_lowercase();
        track.search_text = format!(
            "{} {} {}",
            track.search_title,
            track.artist,
            track.tags.join(" ")
        )
        .to_lowercase();
    }
    Ok(catalog.tracks)
}

pub(crate) async fn download(hash: String, preview: bool) -> Result<tempfile::TempPath> {
    ensure!(is_hash(&hash), "invalid music asset identity");
    let limit = if preview {
        MAX_PREVIEW_BYTES
    } else {
        MAX_TRACK_BYTES
    };
    let bytes = fetch(&format!("media/{hash}.mp3"), limit, Duration::from_secs(45)).await?;
    ensure!(
        format!("{:x}", Sha256::digest(&bytes)) == hash,
        "music download checksum mismatch"
    );
    let mut file = tempfile::Builder::new().suffix(".mp3").tempfile()?;
    file.write_all(&bytes).context("save music download")?;
    Ok(file.into_temp_path())
}

impl EditorWindow {
    pub(crate) fn open_online_music(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.audio_library.online.cancel_preview();
        if self.audio_library.online.input.is_none() {
            let input = cx.new(|cx| ui::TextInputState::single_line(window, cx));
            input.update(cx, |input, _| {
                input.set_placeholder(tr::editor::audio::library::online_search());
            });
            let subscription =
                cx.subscribe_in(&input, window, |this: &mut Self, _, event, _, cx| {
                    if matches!(event, ui::TextInputEvent::Changed) {
                        this.audio_library.online.shown = PAGE_SIZE;
                        cx.notify();
                    }
                });
            self.push_text_subscription(subscription);
            self.audio_library.online.input = Some(input);
        }
        self.audio_library.online.shown = PAGE_SIZE;
        self.load_online_music(cx);
    }

    fn load_online_music(&mut self, cx: &mut Context<Self>) {
        if self.audio_library.online.loading {
            return;
        }
        self.audio_library.online.loading = true;
        self.audio_library.online.failed = false;
        cx.spawn(async |view, cx| {
            let result = gpui_tokio::Tokio::spawn(cx, async {
                let bytes =
                    fetch("catalog.json", MAX_CATALOG_BYTES, Duration::from_secs(12)).await?;
                read_catalog(&bytes)
            })
            .await;
            view.update(cx, |this, cx| {
                let online = &mut this.audio_library.online;
                online.loading = false;
                match result {
                    Ok(Ok(tracks)) => online.tracks = Some(tracks),
                    _ => online.failed = true,
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    fn audition_online_music(&mut self, track: OnlineTrack, cx: &mut Context<Self>) {
        let id = format!("online-{}", track.id);
        let was_pending = self.audio_library.online.preview.as_deref() == Some(track.id.as_str());
        self.audio_library.online.cancel_preview();
        if was_pending
            || self
                .library_audition
                .as_ref()
                .is_some_and(|audition| audition.id.as_ref() == id)
        {
            self.library_audition = None;
            cx.notify();
            return;
        }
        self.library_audition = None;
        self.audio_library.audition_failed = false;
        self.audio_library.online.preview = Some(track.id.clone());
        let generation = self.audio_picker_generation;
        let preview_generation = self.audio_library.online.preview_generation;
        cx.spawn(async move |view, cx| {
            let result = gpui_tokio::Tokio::spawn(cx, download(track.preview, true)).await;
            view.update(cx, |this, cx| {
                if this.audio_picker.is_none()
                    || this.audio_picker_generation != generation
                    || this.audio_library.online.preview_generation != preview_generation
                {
                    return;
                }
                this.audio_library.online.preview = None;
                if let Ok(Ok(file)) = result {
                    this.play_downloaded_audition(id.into(), file, cx);
                } else {
                    this.audio_library.audition_failed = true;
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    pub(crate) fn render_online_music(
        &self,
        importing: bool,
        invalid_position: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = self.theme;
        let online = &self.audio_library.online;
        let mut header = div().flex().flex_col().gap(gpui::px(8.));
        let query = online
            .input
            .as_ref()
            .map(|input| input.read(cx).text().trim().to_lowercase())
            .unwrap_or_default();
        if let Some(input) = &online.input {
            let clear = input.clone();
            header = header.child(
                div()
                    .flex()
                    .items_center()
                    .gap(gpui::px(4.))
                    .child(ui::TextInput::search(&theme, "online-music-query", input))
                    .when(!query.is_empty(), |row| {
                        row.child(
                            ui::EditorButton::plain(&theme, "clear-online-music-query")
                                .left_icon("icons/x.svg")
                                .width(gpui::px(32.))
                                .height(gpui::px(36.))
                                .tooltip(&theme, tr::editor::audio::library::online_clear())
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    clear.update(cx, |input, cx| {
                                        input.set_text("", cx);
                                        input.focus_handle().focus(window, cx);
                                    });
                                    this.audio_library.online.shown = PAGE_SIZE;
                                    cx.notify();
                                })),
                        )
                    }),
            );
        }
        let mut results = div()
            .id("online-music-results")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap(gpui::px(4.));
        if online.failed {
            header = header.child(
                ui::EditorButton::plain(&theme, "reload-online-music")
                    .tinted(&theme)
                    .left_icon("icons/refresh-cw.svg")
                    .label(tr::editor::audio::library::online_retry())
                    .on_click(cx.listener(|this, _, _, cx| this.load_online_music(cx))),
            );
        }
        let tracks = online.tracks.as_deref().unwrap_or_default();
        let terms = query.split_whitespace().collect::<Vec<_>>();
        let mut matching = tracks
            .iter()
            .filter(|track| terms.iter().all(|term| track.search_text.contains(term)))
            .collect::<Vec<_>>();
        if !query.is_empty() {
            matching.sort_by_key(|track| {
                if track.search_title == query {
                    0
                } else if track.search_title.starts_with(&query) {
                    1
                } else {
                    2
                }
            });
        }
        let status: Option<SharedString> = if importing {
            Some(tr::editor::audio::library::online_adding().into())
        } else if online.loading && tracks.is_empty() {
            Some(tr::editor::audio::library::metadata_loading().into())
        } else if online.tracks.is_some() {
            Some(tr::editor::audio::library::online_count(matching.len()).into())
        } else {
            None
        };
        header = header.children(status.map(|status| {
            div()
                .text_size(gpui::px(12.))
                .text_color(Hsla::from(theme.editor.text_2))
                .child(status)
        }));
        for &track in matching.iter().take(online.shown) {
            let id = format!("online-{}", track.id);
            let playing = self
                .library_audition
                .as_ref()
                .is_some_and(|audition| audition.id.as_ref() == id);
            let pending = online.preview.as_deref() == Some(track.id.as_str());
            let preview = track.clone();
            let selected = track.clone();
            results = results.child(
                div()
                    .flex()
                    .flex_shrink_0()
                    .items_center()
                    .gap(gpui::px(8.))
                    .px(gpui::px(4.))
                    .rounded(gpui::px(7.))
                    .min_h(gpui::px(48.))
                    .when(playing || pending, |row| {
                        row.bg(Hsla::from(theme.editor.accent).opacity(0.10))
                    })
                    .child(
                        ui::EditorButton::plain(
                            &theme,
                            SharedString::from(format!("preview-{id}")),
                        )
                        .height(gpui::px(34.))
                        .padding_x(gpui::px(5.))
                        .text_size(gpui::px(12.))
                        .left_icon(if playing || pending {
                            "icons/square.svg"
                        } else {
                            "icons/play.svg"
                        })
                        .label(if playing || pending {
                            tr::editor::audio::library::online_stop()
                        } else {
                            tr::editor::audio::library::online_preview()
                        })
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.audition_online_music(preview.clone(), cx)
                        })),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .truncate()
                                    .text_size(gpui::px(13.))
                                    .font_weight(gpui::FontWeight::MEDIUM)
                                    .child(SharedString::from(track.title.clone())),
                            )
                            .child(
                                div()
                                    .truncate()
                                    .text_size(gpui::px(12.))
                                    .text_color(Hsla::from(theme.editor.text_2))
                                    .child(SharedString::from(format!(
                                        "{} · {}",
                                        track.artist,
                                        crate::editor_timeline::format_time(track.duration)
                                    ))),
                            )
                            .when(!track.tags.is_empty(), |info| {
                                info.child(
                                    div()
                                        .truncate()
                                        .text_size(gpui::px(11.))
                                        .text_color(Hsla::from(theme.editor.text_2))
                                        .child(SharedString::from(track.tags.join(" · "))),
                                )
                            }),
                    )
                    .child(
                        ui::EditorButton::plain(&theme, SharedString::from(format!("add-{id}")))
                            .height(gpui::px(34.))
                            .padding_x(gpui::px(8.))
                            .text_size(gpui::px(12.))
                            .tinted(&theme)
                            .left_icon("icons/plus.svg")
                            .label(tr::editor::audio::library::online_add())
                            .disabled(importing || invalid_position)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.add_online_music(selected.clone(), window, cx)
                            })),
                    ),
            );
        }
        if matching.len() > online.shown {
            results = results.child(
                ui::EditorButton::plain(&theme, "more-online-music")
                    .tinted(&theme)
                    .label(tr::editor::audio::library::online_more())
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.audio_library.online.shown += PAGE_SIZE;
                        cx.notify();
                    })),
            );
        }
        if online.tracks.is_some() && matching.is_empty() {
            results = results.child(tr::editor::audio::library::online_empty());
        }
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .gap(gpui::px(8.))
            .child(header.flex_shrink_0())
            .child(results)
            .into_any_element()
    }
}
