//! [INPUT]: 编辑器时间线、麦克风输入、配音素材。
//! [OUTPUT]: 配音控件、同步录音和带初始增益的自动落轨。
//! [POS]: 协调预览时钟与可恢复配音素材。
//! [PROTOCOL]: Update this header on change, then check AGENTS.md

mod capture;
mod panel;

use std::{
    ops::Range,
    path::PathBuf,
    sync::{Arc, Weak},
};

use cap_editor::EditorInstance;
use cap_project::AudioTrackSegment;
use gpui::{AppContext as _, AsyncWindowContext, Context, Entity, Hsla, Window};
use tokio::sync::{Mutex, watch};
use uuid::Uuid;

use crate::{
    editor_audio::{record_audio_asset, replace_audio_asset},
    editor_window::{EDITOR_PREVIEW_FPS, EditorWindow},
    feeds::Feeds,
    i18n::tr,
    ui,
    voiceover_recording::{
        self, DiscoveredTake, ReadyTake, TakeState, VoiceoverClock, VoiceoverSession,
        VoiceoverStatus,
    },
};

type Session = Arc<Mutex<Option<VoiceoverSession>>>;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Phase {
    #[default]
    Idle,
    Starting,
    Recording,
    Paused,
    Finalizing,
    SourceFinalizing,
    Recovering,
    Ready,
    Error,
}

impl Phase {
    fn busy(self) -> bool {
        matches!(
            self,
            Self::Starting
                | Self::Recording
                | Self::Paused
                | Self::Finalizing
                | Self::SourceFinalizing
                | Self::Recovering
        )
    }
}

#[derive(Debug, PartialEq)]
enum StatusAction {
    Continue,
    Recover,
    Failed,
    Cancelled,
    Ignore,
}

fn observe_recording_phase(phase: &mut Phase, state: TakeState) -> StatusAction {
    if !matches!(
        phase,
        Phase::Recording | Phase::Paused | Phase::SourceFinalizing
    ) {
        return StatusAction::Ignore;
    }
    match state {
        TakeState::Finalizing => {
            *phase = Phase::SourceFinalizing;
            StatusAction::Continue
        }
        TakeState::Ready => {
            *phase = Phase::Ready;
            StatusAction::Recover
        }
        TakeState::Failed => {
            *phase = Phase::Error;
            StatusAction::Failed
        }
        TakeState::Cancelled => {
            *phase = Phase::Idle;
            StatusAction::Cancelled
        }
        _ => StatusAction::Continue,
    }
}

fn audio_destination(
    project: &cap_project::ProjectConfiguration,
    path: &str,
    lane: u32,
    next_lane: u32,
    start: f64,
    total: f64,
    duration: f64,
) -> Option<(u32, std::ops::Range<f64>)> {
    let segments = project.timeline.as_ref()?.audio_segments.as_slice();
    if let Some(segment) = segments.iter().find(|segment| segment.path == path) {
        return Some((segment.track, segment.start..segment.end));
    }
    crate::editor_audio::audio_import_placement(segments, lane, next_lane, start, total, duration)
}

#[derive(Clone)]
struct Ticket {
    generation: u64,
    project: PathBuf,
    instance: Weak<EditorInstance>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Placement {
    start: f64,
    lane: u32,
}

fn placement(
    project: &cap_project::ProjectConfiguration,
    start: f64,
    duration: f64,
    lane: u32,
    next_lane: u32,
) -> Option<Placement> {
    let segments = project.timeline.as_ref()?.audio_segments.as_slice();
    let (lane, range) = crate::editor_audio::audio_import_placement(
        segments,
        lane,
        next_lane,
        start,
        duration,
        (duration - start).max(1.),
    )?;
    (lane <= 4095).then_some(Placement {
        start: range.start,
        lane,
    })
}

struct MicrophonePreview;

struct Panel {
    ticket: Ticket,
    placement: Placement,
    origin: f64,
    selected_range: Option<Range<f64>>,
    replacement: Option<AudioTrackSegment>,
    use_range: bool,
    follow_picture: bool,
    sync: Option<Arc<VoiceoverClock>>,
    countdown: Option<u8>,
    phase: Phase,
    devices: Vec<String>,
    selected: Option<String>,
    device_menu: bool,
    meter: Entity<ui::MicrophoneLevel>,
    _microphone_preview: Entity<MicrophonePreview>,
    session: Option<Session>,
    take_id: Option<Uuid>,
    startup_cancel: Option<watch::Sender<bool>>,
    status: Option<VoiceoverStatus>,
    ready: Option<ReadyTake>,
    takes: Vec<DiscoveredTake>,
    recovery_open: bool,
    pause_pending: bool,
    close_after_cancel: bool,
    confirming: bool,
    error: Option<String>,
}

#[derive(Default)]
pub(crate) struct VoiceoverUi {
    generation: u64,
    panel: Option<Panel>,
}

impl VoiceoverUi {
    pub(crate) fn is_open(&self) -> bool {
        self.panel.is_some()
    }
    pub(crate) fn is_busy(&self) -> bool {
        self.panel.as_ref().is_some_and(|panel| panel.phase.busy())
    }

    pub(crate) fn seek_changed(&mut self, time: f64) {
        if let Some(panel) = self.panel.as_mut()
            && matches!(panel.phase, Phase::Idle | Phase::Error)
            && panel.replacement.is_none()
        {
            panel.origin = time;
        }
    }

    pub(crate) fn waveform(&self) -> Option<crate::editor_timeline::LiveVoiceover> {
        let panel = self.panel.as_ref()?;
        if !matches!(
            panel.phase,
            Phase::Recording | Phase::Paused | Phase::Finalizing | Phase::SourceFinalizing
        ) {
            return None;
        }
        let status = panel.status.as_ref()?;
        if status.peaks.is_empty() {
            return None;
        }
        Some(crate::editor_timeline::LiveVoiceover {
            start: panel.placement.start,
            end: panel.placement.start + status.duration_estimate.as_secs_f64(),
            lane: panel.placement.lane,
            peaks: status.peaks.clone(),
            paused: panel.phase == Phase::Paused,
        })
    }
}

impl Panel {
    fn range(&self, total: f64) -> Range<f64> {
        let bounds = self
            .replacement
            .as_ref()
            .map(|segment| segment.start..segment.end)
            .or_else(|| self.selected_range.clone().filter(|_| self.use_range))
            .unwrap_or(self.origin..total);
        bounds.start..bounds.end.min(total)
    }
}

fn frame_floor(time: f64) -> f64 {
    let fps = f64::from(EDITOR_PREVIEW_FPS);
    (time * fps + 1e-7).floor() / fps
}

impl EditorWindow {
    fn voiceover_matches(&self, ticket: &Ticket) -> bool {
        self.voiceover
            .panel
            .as_ref()
            .is_some_and(|panel| panel.ticket.generation == ticket.generation)
            && self.project_path == ticket.project
            && ticket
                .instance
                .upgrade()
                .zip(self.instance.as_ref())
                .is_some_and(|(old, current)| Arc::ptr_eq(&old, current))
    }

    pub(crate) fn open_voiceover(
        &mut self,
        lane: u32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_voiceover_panel(lane, None, window, cx);
    }

    pub(crate) fn open_existing_voiceover(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let segment = self
            .project
            .timeline
            .as_ref()
            .and_then(|timeline| timeline.audio_segments.get(index))
            .filter(|segment| segment.path.starts_with("content/voiceovers/"))
            .cloned();
        if let Some(segment) = segment {
            self.open_voiceover_panel(segment.track, Some(segment), window, cx);
        }
    }

    fn open_voiceover_panel(
        &mut self,
        lane: u32,
        replacement: Option<AudioTrackSegment>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.voiceover.is_open()
            || self.busy_reason().is_some()
            || self.audio_import_pending.is_some()
        {
            return;
        }
        let Some(instance) = self.instance.as_ref() else {
            return;
        };
        let origin = replacement
            .as_ref()
            .map_or(self.playhead, |segment| segment.start);
        let selected_range = self.voiceover_selection_range();
        let has_picture = self.total_duration() - origin >= 1.0 / f64::from(EDITOR_PREVIEW_FPS);
        let use_range = !has_picture && selected_range.is_some();
        let Some(reserved) = placement(
            &self.project,
            origin,
            self.total_duration(),
            lane,
            self.tracks.audio,
        ) else {
            self.show_notice(tr::editor::voiceover::move_playhead(), window, cx);
            return;
        };
        let instance = Arc::downgrade(instance);
        if let Err(error) = self.flush_pending_saves(cx) {
            self.show_notice(error, window, cx);
            return;
        }
        self.library_audition = None;
        self.audio_library.audition_failed = false;
        self.stop_playback(cx);
        self.seek_to_time(origin, cx);
        self.audio_picker = None;
        self.cancel_asset_drag(cx);
        let feeds = Feeds::global(cx);
        let level = feeds.read(cx).mic_level().clone();
        let selected = feeds.read(cx).microphone.clone();
        let meter = cx.new(|cx| {
            ui::MicrophoneLevel::new(ui::MicrophoneLevelKind::Voiceover, &level, window, cx)
        });
        let theme = self.theme;
        meter.update(cx, |meter, cx| {
            meter.configure(
                true,
                Hsla::from(theme.gray_11),
                Hsla::from(theme.gray_4),
                cx,
            )
        });
        self.voiceover.generation = self.voiceover.generation.wrapping_add(1);
        let ticket = Ticket {
            generation: self.voiceover.generation,
            project: self.project_path.clone(),
            instance,
        };
        feeds.update(cx, |feeds, cx| {
            feeds.acquire_mic_preview(selected.clone(), cx)
        });
        let preview = cx.new(|cx| {
            cx.on_release(move |_: &mut MicrophonePreview, cx| {
                feeds.update(cx, |feeds, cx| feeds.release_mic_tap(cx));
            })
            .detach();
            MicrophonePreview
        });
        self.voiceover.panel = Some(Panel {
            ticket: ticket.clone(),
            placement: Placement {
                start: origin,
                lane: replacement
                    .as_ref()
                    .map_or(reserved.lane, |segment| segment.track),
            },
            origin,
            selected_range,
            use_range,
            follow_picture: replacement.is_some() || has_picture || use_range,
            replacement,
            sync: None,
            countdown: None,
            phase: Phase::Idle,
            devices: Vec::new(),
            selected,
            device_menu: false,
            meter,
            _microphone_preview: preview,
            session: None,
            take_id: None,
            startup_cancel: None,
            status: None,
            ready: None,
            takes: Vec::new(),
            recovery_open: false,
            pause_pending: false,
            close_after_cancel: false,
            confirming: false,
            error: None,
        });
        window.focus(&self.focus_handle_for_menu(), cx);
        cx.spawn_in(window, async move |this, cx: &mut AsyncWindowContext| {
            let project = ticket.project.clone();
            let (devices, takes) = cx
                .background_executor()
                .spawn(async move {
                    (
                        cap_recording::MicrophoneFeed::list_names(),
                        voiceover_recording::list_takes(&project)
                            .map_err(|error| error.to_string()),
                    )
                })
                .await;
            let _ = this.update_in(cx, |this, _, cx| {
                if !this.voiceover_matches(&ticket) {
                    return;
                }
                let panel = this.voiceover.panel.as_mut().unwrap();
                if panel.selected.is_none() {
                    panel.selected = devices.first().cloned();
                }
                panel.devices = devices;
                let selected = panel.selected.clone();
                Feeds::global(cx).update(cx, |feeds, cx| feeds.set_microphone(selected, cx));
                match takes {
                    Ok(takes) => panel.takes = takes,
                    Err(error) => panel.error = Some(error),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn attach_voiceover(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(panel) = self.voiceover.panel.as_ref() else {
            return;
        };
        if panel.phase != Phase::Ready || !self.voiceover_matches(&panel.ticket) {
            return;
        }
        let Some(mut take) = panel.ready.clone() else {
            return;
        };
        let replacement = panel.replacement.clone();
        self.stop_playback(cx);
        self.library_audition = None;
        let Some((mut lane, mut range)) = audio_destination(
            &self.project,
            &take.relative_path(),
            take.manifest().lane,
            self.tracks.audio,
            take.manifest().output_start,
            self.total_duration(),
            take.audio().duration,
        ) else {
            self.voiceover.panel.as_mut().unwrap().error =
                Some(tr::editor::voiceover::move_playhead().into());
            cx.notify();
            return;
        };
        let result = (|| -> Result<(), String> {
            if !take.is_in_project(&self.project) {
                let name = tr::editor::voiceover::asset_name(clock(range.start));
                if let Some(replacement) = replacement {
                    let path = take.relative_path();
                    let duration = take.audio().duration;
                    let record = record_audio_asset(&self.project_path, &path, name.as_ref())?;
                    let updated = self.edit_timeline_project(
                        |project| {
                            let Some(timeline) = project.timeline.as_mut() else {
                                return false;
                            };
                            let Some(index) = timeline.audio_segments.iter().position(|segment| {
                                segment.path == replacement.path
                                    && segment.track == replacement.track
                                    && segment.start == replacement.start
                                    && segment.end == replacement.end
                            }) else {
                                return false;
                            };
                            if !replace_audio_asset(
                                timeline,
                                index,
                                path,
                                name.to_string(),
                                duration,
                            ) {
                                return false;
                            }
                            let segment = &timeline.audio_segments[index];
                            lane = segment.track;
                            range = segment.start..segment.end;
                            true
                        },
                        window,
                        cx,
                    );
                    if !updated {
                        return Err(tr::editor::voiceover::move_playhead().into());
                    }
                    self.remember_project_asset(record, cx);
                } else {
                    self.commit_project_audio_asset(
                        crate::asset_library::AssetRecord {
                            path: take.relative_path(),
                            name: name.into(),
                            kind: crate::asset_library::AssetKind::Audio,
                            source: None,
                            rendition: None,
                        },
                        take.audio().duration,
                        range.start,
                        Some(lane),
                        take.audio().initial_gain_db(),
                        window,
                        cx,
                    )?;
                }
            }
            self.flush_pending_saves(cx)?;
            take.mark_attached().map_err(|error| format!("{error:#}"))
        })();
        let (start, end) = (range.start, range.end);
        match result {
            Ok(()) => {
                self.voiceover.panel = None;
                self.reveal_timeline_range(start, end, cx);
                self.reveal_timeline_row(crate::editor_timeline::TrackKind::Audio, lane, cx);
                self.show_notice(tr::editor::voiceover::added(), window, cx);
                self.focus_root(window, cx);
            }
            Err(error) => {
                let panel = self.voiceover.panel.as_mut().unwrap();
                panel.ready = Some(take);
                panel.error = Some(error);
            }
        }
        cx.notify();
    }

    fn reload_voiceover_takes(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ticket) = self
            .voiceover
            .panel
            .as_ref()
            .map(|panel| panel.ticket.clone())
        else {
            return;
        };
        cx.spawn_in(window, async move |this, cx: &mut AsyncWindowContext| {
            let project = ticket.project.clone();
            let result = cx
                .background_executor()
                .spawn(async move {
                    voiceover_recording::list_takes(&project).map_err(|error| error.to_string())
                })
                .await;
            let _ = this.update_in(cx, |this, _, cx| {
                if !this.voiceover_matches(&ticket) {
                    return;
                }
                let panel = this.voiceover.panel.as_mut().unwrap();
                match result {
                    Ok(takes) => panel.takes = takes,
                    Err(error) => panel.error = Some(error),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn recover_voiceover(&mut self, id: Uuid, window: &mut Window, cx: &mut Context<Self>) {
        let Some(panel) = self.voiceover.panel.as_ref() else {
            return;
        };
        if panel.phase.busy() {
            return;
        }
        self.stop_playback(cx);
        self.library_audition = None;
        self.voiceover.generation = self.voiceover.generation.wrapping_add(1);
        let panel = self.voiceover.panel.as_mut().unwrap();
        panel.ticket.generation = self.voiceover.generation;
        panel.phase = Phase::Recovering;
        panel.error = None;
        let ticket = panel.ticket.clone();
        cx.spawn_in(window, async move |this, cx: &mut AsyncWindowContext| {
            let project = ticket.project.clone();
            let result = gpui_tokio::Tokio::spawn(cx, async move {
                voiceover_recording::recover_take(project, id)
                    .await
                    .map_err(|error| format!("{error:#}"))
            })
            .await
            .unwrap_or_else(|error| Err(error.to_string()));
            let _ = this.update_in(cx, |this, window, cx| {
                if !this.voiceover_matches(&ticket) {
                    return;
                }
                let total = this.total_duration();
                let panel = this.voiceover.panel.as_mut().unwrap();
                match result {
                    Ok(take) => {
                        let start = take.manifest().output_start;
                        let lane = take.manifest().lane;
                        if panel.take_id != Some(id) {
                            panel.replacement = None;
                            panel.selected_range = None;
                            panel.use_range = false;
                            panel.origin = start;
                            panel.follow_picture = start + take.audio().duration <= total + 0.05;
                        }
                        panel.take_id = Some(id);
                        panel.placement = Placement { start, lane };
                        panel.ready = Some(take);
                        panel.phase = Phase::Ready;
                        this.attach_voiceover(window, cx);
                    }
                    Err(error) => {
                        panel.error = Some(error);
                        panel.phase = Phase::Error;
                    }
                }
                this.reload_voiceover_takes(window, cx);
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    pub(crate) fn close_voiceover(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.library_audition = None;
        let Some(panel) = self.voiceover.panel.as_ref() else {
            return;
        };
        if panel.confirming
            || matches!(
                panel.phase,
                Phase::Finalizing | Phase::SourceFinalizing | Phase::Recovering
            )
        {
            return;
        }
        if panel.phase == Phase::Starting {
            self.voiceover.panel.as_mut().unwrap().close_after_cancel = true;
            self.finish_voiceover(true, window, cx);
            return;
        }
        if !panel.phase.busy() {
            self.stop_playback(cx);
            self.voiceover.panel = None;
            self.focus_root(window, cx);
            cx.notify();
            return;
        }
        let ticket = panel.ticket.clone();
        self.voiceover.panel.as_mut().unwrap().confirming = true;
        #[cfg(target_os = "linux")]
        let answer = crate::editor_modal::confirm_action(
            tr::editor::voiceover::cancel_title(),
            tr::editor::voiceover::cancel_message(),
            tr::editor::voiceover::discard(),
            tr::editor::voiceover::keep_recording(),
            window,
            cx,
        );
        cx.spawn_in(window, async move |this, cx: &mut AsyncWindowContext| {
            #[cfg(target_os = "linux")]
            let confirmed = answer.await;
            #[cfg(not(target_os = "linux"))]
            let confirmed = crate::platform::confirm_dialog(
                tr::editor::voiceover::cancel_title(),
                tr::editor::voiceover::cancel_message(),
                tr::editor::voiceover::discard(),
                tr::editor::voiceover::keep_recording(),
                false,
            );
            let _ = this.update_in(cx, |this, window, cx| {
                if !this.voiceover_matches(&ticket) {
                    return;
                }
                this.voiceover.panel.as_mut().unwrap().confirming = false;
                if confirmed {
                    this.voiceover.panel.as_mut().unwrap().close_after_cancel = true;
                    this.finish_voiceover(true, window, cx);
                } else if this
                    .voiceover
                    .panel
                    .as_ref()
                    .is_some_and(|panel| panel.phase == Phase::SourceFinalizing)
                {
                    this.finish_voiceover(false, window, cx);
                }
                cx.notify();
            });
        })
        .detach();
    }
}

fn clock(seconds: f64) -> String {
    let value = if seconds.is_finite() {
        seconds.max(0.)
    } else {
        0.
    };
    let minutes = (value / 60.).floor() as u64;
    format!("{minutes:02}:{:05.2}", value % 60.)
}

#[cfg(test)]
mod tests;
