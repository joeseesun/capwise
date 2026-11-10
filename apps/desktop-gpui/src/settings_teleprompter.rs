use gpui::{AnyElement, AppContext as _, Context, IntoElement, ParentElement, Styled, div, px};

use crate::{
    app_windows,
    i18n::tr::{self, settings::teleprompter as copy},
    llm::{self, ContentModel},
    settings_window::{MenuKind, Page, SettingsWindow},
    store::{self, TeleprompterState},
    ui,
};

const MODEL: &str = "scriptModel";
const VOICE_FOLLOW: &str = "defaultVoiceFollow";
const AUTO_START: &str = "startWithRecording";
const NARRATION_SPEED: &str = "narrationSpeed";
const SPEEDS: &[u32] = &[60, 90, 120, 150, 180, 210, 240, 270, 300, 350];

#[derive(Default)]
pub(crate) struct TeleprompterSettings {
    models: Vec<ContentModel>,
    save_failed: bool,
    #[cfg(target_os = "macos")]
    reading: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
    #[cfg(target_os = "macos")]
    read_failed: bool,
}

impl TeleprompterSettings {
    pub(crate) fn stop_preview(&mut self) {
        #[cfg(target_os = "macos")]
        if let Some(reading) = self.reading.take() {
            reading.store(true, std::sync::atomic::Ordering::Relaxed);
        }
    }
}

impl Drop for TeleprompterSettings {
    fn drop(&mut self) {
        self.stop_preview();
    }
}

fn preference(key: &str) -> Option<serde_json::Value> {
    store::store_section(store::TELEPROMPTER).remove(key)
}

fn model_id() -> Option<String> {
    preference(MODEL).and_then(|value| value.as_str().map(str::to_owned))
}

pub(crate) fn has_explicit_model() -> bool {
    model_id().is_some()
}

fn configured_models() -> impl Iterator<Item = ContentModel> {
    llm::configured()
        .into_iter()
        .filter(|provider| provider.ready())
        .map(ContentModel::Provider)
        .chain(llm::agent_content_models())
}

pub(crate) fn script_model() -> Option<ContentModel> {
    match model_id() {
        Some(id) => configured_models().find(|model| model.id() == id),
        None => llm::default_content_model(),
    }
}

pub(crate) fn default_voice_follow() -> bool {
    cfg!(target_os = "macos")
        && preference(VOICE_FOLLOW)
            .and_then(|value| value.as_bool())
            .unwrap_or(false)
}

pub(crate) fn auto_start() -> bool {
    preference(AUTO_START)
        .and_then(|value| value.as_bool())
        .unwrap_or(true)
}

fn narration_speed() -> u32 {
    preference(NARRATION_SPEED)
        .and_then(|value| value.as_u64())
        .and_then(|value| u32::try_from(value).ok())
        .filter(|value| SPEEDS.contains(value))
        .unwrap_or(150)
}

fn label(model: &ContentModel) -> String {
    format!("{} · {}", model.provider_name(), model.model_name())
}

fn mode_label(voice: bool) -> &'static str {
    if voice {
        tr::dialog::teleprompter::follow::title()
    } else {
        copy::steady()
    }
}

impl SettingsWindow {
    pub(crate) fn prepare_prompter_models(&mut self) {
        self.teleprompter.models = configured_models().collect();
    }

    pub(crate) fn prompter_menu_items(&self, kind: MenuKind) -> Vec<ui::MenuItem> {
        match kind {
            MenuKind::PrompterModel => {
                let selected = model_id();
                std::iter::once(ui::MenuItem::new(
                    copy::use_content_model(),
                    selected.is_none(),
                ))
                .chain(self.teleprompter.models.iter().map(|model| {
                    ui::MenuItem::new(
                        label(model),
                        selected.as_ref().is_some_and(|id| model.id() == *id),
                    )
                }))
                .collect()
            }
            MenuKind::PrompterMode => [false, true]
                .into_iter()
                .filter(|voice| !*voice || cfg!(target_os = "macos"))
                .map(|voice| ui::MenuItem::new(mode_label(voice), voice == default_voice_follow()))
                .collect(),
            MenuKind::PrompterSpeed | MenuKind::PrompterNarrationSpeed => {
                let current = if kind == MenuKind::PrompterSpeed {
                    TeleprompterState::load().words_per_minute
                } else {
                    narration_speed()
                };
                SPEEDS
                    .iter()
                    .map(|&speed| {
                        ui::MenuItem::new(
                            tr::dialog::teleprompter::words_per_minute(speed),
                            speed == current,
                        )
                    })
                    .collect()
            }
            _ => Vec::new(),
        }
    }

    fn save_prompter_option(
        &mut self,
        key: &str,
        value: serde_json::Value,
        cx: &mut Context<Self>,
    ) -> bool {
        let saved = store::set_store_setting(store::TELEPROMPTER, key, value);
        self.teleprompter.save_failed = !saved;
        cx.notify();
        saved
    }

    pub(crate) fn choose_prompter_option(
        &mut self,
        kind: MenuKind,
        index: usize,
        cx: &mut Context<Self>,
    ) {
        match kind {
            MenuKind::PrompterModel => {
                let value = if index == 0 {
                    serde_json::Value::Null
                } else if let Some(model) = self.teleprompter.models.get(index - 1) {
                    model.id().into()
                } else {
                    return;
                };
                self.save_prompter_option(MODEL, value, cx);
            }
            MenuKind::PrompterMode if index == 0 || (index == 1 && cfg!(target_os = "macos")) => {
                self.save_prompter_option(VOICE_FOLLOW, (index == 1).into(), cx);
            }
            MenuKind::PrompterSpeed | MenuKind::PrompterNarrationSpeed => {
                let Some(&speed) = SPEEDS.get(index) else {
                    return;
                };
                if kind == MenuKind::PrompterSpeed {
                    if self.save_prompter_option(
                        TeleprompterState::WORDS_PER_MINUTE,
                        speed.into(),
                        cx,
                    ) {
                        cx.defer(move |cx| app_windows::apply_teleprompter_speed(speed, cx));
                    }
                } else {
                    self.teleprompter.stop_preview();
                    self.save_prompter_option(NARRATION_SPEED, speed.into(), cx);
                }
            }
            _ => {}
        }
    }

    pub(crate) fn render_teleprompter_settings(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let current_model = model_id();
        let model_label = if current_model.is_some() {
            script_model()
                .as_ref()
                .map_or_else(|| copy::model_unavailable_short().into(), label)
        } else {
            copy::use_content_model().into()
        };
        let model_control = self
            .select(
                "prompter-script-model",
                model_label,
                MenuKind::PrompterModel,
                cx,
            )
            .into_any_element();
        let manage_models = self
            .button(
                "prompter-manage-models",
                (ui::ButtonVariant::Gray, None),
                tr::settings::nav::transcription(),
                false,
                cx,
                |this, window, cx| this.set_page(Page::Transcription, window, cx),
            )
            .into_any_element();
        let writing = self.rows(vec![self.setting_row(copy::model(), None, model_control)]);
        let voice = default_voice_follow();
        let speed = TeleprompterState::load().words_per_minute;
        let speed_control = self
            .select(
                "prompter-speed",
                tr::dialog::teleprompter::words_per_minute(speed),
                MenuKind::PrompterSpeed,
                cx,
            )
            .into_any_element();
        let mut rows = Vec::new();
        if cfg!(target_os = "macos") {
            let mode = self
                .select(
                    "prompter-mode",
                    mode_label(voice),
                    MenuKind::PrompterMode,
                    cx,
                )
                .into_any_element();
            rows.push(self.setting_row(copy::mode(), Some(copy::mode_help()), mode));
        }
        rows.push(self.setting_row(copy::speed(), Some(copy::speed_help()), speed_control));
        let auto_start = self
            .toggle(
                "prompter-auto-start",
                copy::auto_start(),
                auto_start(),
                cx,
                |this, cx| {
                    this.save_prompter_option(
                        AUTO_START,
                        (!crate::settings_teleprompter::auto_start()).into(),
                        cx,
                    );
                },
            )
            .into_any_element();
        rows.push(self.setting_row(
            copy::auto_start(),
            Some(copy::auto_start_help()),
            auto_start,
        ));
        let scrolling = self.rows(rows);
        let mut sections = vec![
            self.section(
                copy::scrolling(),
                None,
                None,
                vec![scrolling.into_any_element()],
            )
            .into_any_element(),
            self.section(
                copy::writing(),
                Some(copy::writing_help()),
                Some(manage_models),
                vec![writing.into_any_element()],
            )
            .into_any_element(),
        ];
        #[cfg(target_os = "macos")]
        sections.push(self.render_prompter_narration(cx));
        if self.teleprompter.save_failed {
            sections.push(
                div()
                    .text_size(px(12.))
                    .child(copy::save_failed())
                    .into_any_element(),
            );
        }
        sections
    }

    #[cfg(target_os = "macos")]
    fn render_prompter_narration(&self, cx: &mut Context<Self>) -> AnyElement {
        let recording = crate::session::RecordingSession::global(cx).read(cx).phase
            != crate::session::Phase::Idle;
        let reading = self.teleprompter.reading.is_some();
        let speed = self
            .select(
                "prompter-narration-speed",
                tr::dialog::teleprompter::words_per_minute(narration_speed()),
                MenuKind::PrompterNarrationSpeed,
                cx,
            )
            .into_any_element();
        let preview = self
            .button(
                "prompter-narration-preview",
                (
                    ui::ButtonVariant::Gray,
                    Some(if reading {
                        "icons/square.svg"
                    } else {
                        "icons/play.svg"
                    }),
                ),
                if reading {
                    copy::stop_reading()
                } else {
                    copy::read_sample()
                },
                recording,
                cx,
                |this, window, cx| this.preview_prompter_narration(window, cx),
            )
            .into_any_element();
        let rows = self.rows(vec![self.setting_row(copy::narration_speed(), None, speed)]);
        let mut children = vec![rows.into_any_element()];
        if self.teleprompter.read_failed {
            children.push(
                div()
                    .text_size(px(12.))
                    .child(copy::read_failed())
                    .into_any_element(),
            );
        }
        self.section(
            copy::narration(),
            Some(copy::narration_help()),
            Some(preview),
            children,
        )
        .into_any_element()
    }

    #[cfg(target_os = "macos")]
    fn preview_prompter_narration(&mut self, window: &mut gpui::Window, cx: &mut Context<Self>) {
        use std::sync::{Arc, atomic::AtomicBool};
        if self.teleprompter.reading.is_some() {
            self.teleprompter.stop_preview();
            cx.notify();
            return;
        }
        if crate::session::RecordingSession::global(cx).read(cx).phase
            != crate::session::Phase::Idle
        {
            return;
        }
        let state = TeleprompterState::load();
        let script = if state.script.trim().is_empty() {
            tr::settings::speech::sample()
        } else {
            state.script.trim()
        };
        let text = script
            .chars()
            .take(160)
            .collect::<String>()
            .replace("[[", "[ [");
        let speed = narration_speed();
        let active = Arc::new(AtomicBool::new(false));
        self.teleprompter.reading = Some(active.clone());
        self.teleprompter.read_failed = false;
        let cancel = active.clone();
        self.spawn_tokio(
            window,
            cx,
            async move {
                tokio::task::spawn_blocking(move || play_system_voice(&text, speed, &cancel))
                    .await
                    .is_ok_and(|result| result.is_ok())
            },
            move |this, ok, _, _| {
                if this
                    .teleprompter
                    .reading
                    .as_ref()
                    .is_some_and(|current| Arc::ptr_eq(current, &active))
                {
                    this.teleprompter.reading = None;
                    this.teleprompter.read_failed = !ok;
                }
            },
        );
        cx.notify();
    }
}

#[cfg(target_os = "macos")]
fn play_system_voice(
    text: &str,
    speed: u32,
    cancel: &std::sync::atomic::AtomicBool,
) -> std::io::Result<()> {
    use std::{
        io::Write,
        process::{Command, Stdio},
        sync::atomic::Ordering,
        time::{Duration, Instant},
    };

    if cancel.load(Ordering::Relaxed) {
        return Ok(());
    }
    let mut child = Command::new("/usr/bin/say")
        .args(["-r", &speed.to_string(), "-f", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    let result = (|| {
        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(text.as_bytes())?;
        }
        let start = Instant::now();
        while !cancel.load(Ordering::Relaxed) {
            if start.elapsed() >= Duration::from_secs(50) {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "speech timed out",
                ));
            }
            if let Some(status) = child.try_wait()? {
                return if status.success() {
                    Ok(())
                } else {
                    Err(std::io::Error::other(status.to_string()))
                };
            }
            std::thread::sleep(Duration::from_millis(40));
        }
        Ok(())
    })();
    if child.try_wait().ok().flatten().is_none() {
        let _ = child.kill();
        let _ = child.wait();
    }
    result
}
