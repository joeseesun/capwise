//! [INPUT]: 配音会话、播放器和录制状态。
//! [OUTPUT]: 同步开始、暂停、停止及回看。
//! [POS]: 配音侧栏与媒体引擎的协调层。
//! [PROTOCOL]: Update this header on change, then check AGENTS.md

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use cap_editor::PreviewPlayback;
use gpui::{AsyncWindowContext, Context, Window};
use tokio::sync::{Mutex, watch};
use uuid::Uuid;

use crate::{
    editor_window::{EDITOR_PREVIEW_FPS, EditorWindow},
    feeds::Feeds,
    i18n::tr,
    voiceover_recording::{
        TakeState, VoiceoverClock, VoiceoverRequest, VoiceoverSession, VoiceoverStatus,
    },
};

use super::{
    Phase, Placement, StatusAction, Ticket, frame_floor, observe_recording_phase, review_project,
};

impl EditorWindow {
    pub(super) fn start_voiceover(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(panel) = self.voiceover.panel.as_ref() else {
            return;
        };
        if panel.phase.busy() || panel.ready.is_some() {
            return;
        }
        let Some(selected) = panel.selected.clone() else {
            return;
        };
        let ticket = panel.ticket.clone();
        if !self.voiceover_matches(&ticket) {
            return;
        }
        let range = panel.range(self.total_duration());
        let (start, sync) = if panel.follow_picture {
            if (range.end - range.start) < 1.0 / f64::from(EDITOR_PREVIEW_FPS) {
                self.voiceover.panel.as_mut().unwrap().error =
                    Some(tr::editor::voiceover::move_playhead().into());
                cx.notify();
                return;
            }
            (range.start, Some(Arc::new(VoiceoverClock::default())))
        } else {
            (panel.origin, None)
        };
        let lane = panel.placement.lane;
        let end = range.end;
        self.seek_to_time(start, cx);
        if let Err(error) = self.flush_pending_saves(cx) {
            self.voiceover.panel.as_mut().unwrap().error = Some(error);
            cx.notify();
            return;
        }
        self.library_audition = None;
        self.audio_library.audition_failed = false;
        self.stop_playback(cx);
        let feeds = Feeds::global(cx);
        feeds.update(cx, |feeds, cx| {
            feeds.set_microphone(Some(selected.clone()), cx)
        });
        let (ready, actor) = {
            let feeds = feeds.read(cx);
            (feeds.input_readiness().microphone, feeds.mic_actor())
        };
        let (Some(ready), Some(actor)) = (ready, actor) else {
            self.voiceover.panel.as_mut().unwrap().error =
                Some(tr::editor::voiceover::microphone_unavailable().into());
            cx.notify();
            return;
        };
        self.voiceover.generation = self.voiceover.generation.wrapping_add(1);
        let panel = self.voiceover.panel.as_mut().unwrap();
        panel.ticket.generation = self.voiceover.generation;
        panel.placement = Placement { start, lane };
        panel.sync = sync.clone();
        panel.countdown = None;
        let ticket = panel.ticket.clone();
        let request = VoiceoverRequest {
            project_path: self.project_path.clone(),
            id: Uuid::new_v4(),
            output_start: panel.placement.start,
            lane: panel.placement.lane,
        };
        panel.take_id = Some(request.id);
        let (cancel, mut cancelled) = watch::channel(false);
        panel.startup_cancel = Some(cancel);
        panel.phase = Phase::Starting;
        panel.close_after_cancel = false;
        panel.pause_pending = false;
        panel.error = None;
        panel.status = None;
        panel.device_menu = false;
        cx.spawn_in(window, async move |this, cx: &mut AsyncWindowContext| {
            let follows_picture = sync.is_some();
            let setup_sync = sync.clone();
            let mut setup_cancel = cancelled.clone();
            let mut result = gpui_tokio::Tokio::spawn(cx, async move {
                let lock = tokio::select! {
                    _ = setup_cancel.changed() => return Ok(None),
                    result = async {
                        ready.await?;
                        let lock = actor.ask(cap_recording::feeds::microphone::Lock).await.map_err(|error| error.to_string())?;
                        if lock.device_name() != selected { return Err(tr::editor::voiceover::microphone_changed().to_string()); }
                        Ok::<_, String>(Arc::new(lock))
                    } => result?,
                };
                if *setup_cancel.borrow() { return Ok(None); }
                let session = if setup_sync.is_some() {
                    VoiceoverSession::start_with_clock(request, lock, setup_sync).await
                } else {
                    VoiceoverSession::start(request, lock).await
                }.map_err(|error| format!("{error:#}"))?;
                if *setup_cancel.borrow() || setup_cancel.has_changed().is_err() {
                    session.cancel().await.map_err(|error| format!("{error:#}"))?;
                    Ok(None)
                } else { Ok(Some(session)) }
            }).await.unwrap_or_else(|error| Err(error.to_string()));
            if follows_picture && matches!(result, Ok(Some(_))) {
                for count in (1..=3).rev() {
                    if *cancelled.borrow() || !this.update_in(cx, |this, _, cx| {
                        if !this.voiceover_matches(&ticket) {
                            return false;
                        }
                        this.voiceover.panel.as_mut().unwrap().countdown = Some(count);
                        cx.notify();
                        true
                    }).unwrap_or(false) {
                        break;
                    }
                    let timer = cx.background_executor().timer(Duration::from_secs(1));
                    let cancelled_early = tokio::select! {
                        _ = timer => false,
                        _ = cancelled.changed() => true,
                    };
                    if cancelled_early {
                        break;
                    }
                }
                if (*cancelled.borrow() || cancelled.has_changed().is_err())
                    && let Ok(Some(session)) = result
                {
                    result = gpui_tokio::Tokio::spawn(cx, async move {
                        session.cancel().await.map(|()| None).map_err(|error| format!("{error:#}"))
                    }).await.unwrap_or_else(|error| Err(error.to_string()));
                }
            }
            let mut result = Some(result);
            let accepted = this.update_in(cx, |this, window, cx| {
                if !this.voiceover_matches(&ticket) { return false; }
                let panel = this.voiceover.panel.as_mut().unwrap();
                panel.startup_cancel = None;
                panel.countdown = None;
                match result.take().unwrap() {
                    Ok(Some(session)) => {
                        let status = session.subscribe();
                        panel.status = Some(session.status());
                        panel.session = Some(Arc::new(Mutex::new(Some(session))));
                        panel.phase = Phase::Recording;
                        if panel.close_after_cancel {
                            this.finish_voiceover(true, window, cx);
                        } else {
                            this.watch_voiceover(ticket.clone(), status, window, cx);
                            if sync.is_some() && !this.start_voiceover_picture(start, end, cx) {
                                this.finish_voiceover(true, window, cx);
                                if let Some(panel) = this.voiceover.panel.as_mut() {
                                    panel.error = Some(tr::editor::voiceover::picture_stopped().into());
                                }
                            }
                        }
                    }
                    Ok(None) => {
                        let close = panel.close_after_cancel;
                        panel.phase = Phase::Idle;
                        if close {
                            this.voiceover.panel = None;
                            this.focus_root(window, cx);
                        }
                    }
                    Err(error) => {
                        panel.phase = Phase::Error;
                        panel.error = Some(error);
                    }
                }
                cx.notify();
                true
            }).unwrap_or(false);
            if !accepted && let Some(Ok(Some(session))) = result {
                let _ = gpui_tokio::Tokio::spawn(cx, async move { session.cancel().await }).await;
            }
        }).detach();
        cx.notify();
    }

    fn watch_voiceover(
        &mut self,
        ticket: Ticket,
        mut status: watch::Receiver<VoiceoverStatus>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.spawn_in(window, async move |this, cx: &mut AsyncWindowContext| {
            loop {
                let value = status.borrow_and_update().clone();
                let keep = this
                    .update_in(cx, |this, window, cx| {
                        if !this.voiceover_matches(&ticket) {
                            return false;
                        }
                        let panel = this.voiceover.panel.as_mut().unwrap();
                        let action = observe_recording_phase(&mut panel.phase, value.state);
                        if action == StatusAction::Ignore {
                            return false;
                        }
                        panel.status = Some(value.clone());
                        if panel.confirming
                            && matches!(action, StatusAction::Recover | StatusAction::Failed)
                        {
                            panel.phase = Phase::SourceFinalizing;
                            cx.notify();
                            return false;
                        }
                        if value.state == TakeState::Finalizing && panel.sync.is_some() {
                            this.stop_playback(cx);
                        }
                        let panel = this.voiceover.panel.as_mut().unwrap();
                        match action {
                            StatusAction::Recover => {
                                let id = panel.take_id;
                                panel.session = None;
                                if let Some(id) = id {
                                    this.recover_voiceover(id, window, cx);
                                }
                            }
                            StatusAction::Failed => {
                                panel.session = None;
                                panel.error = Some(value.error.clone().unwrap_or_else(|| {
                                    tr::editor::voiceover::recording_ended().into()
                                }));
                                this.reload_voiceover_takes(window, cx);
                            }
                            StatusAction::Cancelled => {
                                panel.session = None;
                            }
                            _ => {}
                        }
                        cx.notify();
                        action == StatusAction::Continue
                    })
                    .unwrap_or(false);
                if !keep || status.changed().await.is_err() {
                    break;
                }
            }
        })
        .detach();
    }

    fn start_voiceover_picture(&mut self, at: f64, end: f64, cx: &mut Context<Self>) -> bool {
        let Some(sync) = self
            .voiceover
            .panel
            .as_ref()
            .and_then(|panel| panel.sync.as_ref())
        else {
            return false;
        };
        let frame_time = frame_floor(at);
        let preview = sync.preview(
            Duration::from_secs_f64((at - frame_time).max(0.0)),
            Duration::from_secs_f64((end - at).max(0.0)),
        );
        self.play_voiceover_preview(frame_time, PreviewPlayback::Voiceover(preview), None, cx)
    }

    pub(crate) fn voiceover_playback_ended(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(panel) = self.voiceover.panel.as_ref() else {
            return;
        };
        if panel.phase != Phase::Recording {
            cx.notify();
            return;
        }
        let Some(sync) = &panel.sync else {
            return;
        };
        let elapsed = sync.elapsed(Instant::now()).as_secs_f64();
        let interrupted =
            panel.placement.start + elapsed + 0.04 < panel.range(self.total_duration()).end;
        if elapsed == 0.0 {
            self.finish_voiceover(true, window, cx);
        } else {
            self.finish_voiceover(false, window, cx);
        }
        if interrupted && let Some(panel) = self.voiceover.panel.as_mut() {
            panel.error = Some(tr::editor::voiceover::picture_stopped().into());
        }
    }

    pub(crate) fn voiceover_play_action(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.voiceover.panel.as_ref().map(|panel| panel.phase) {
            Some(Phase::Idle | Phase::Error) => self.start_voiceover(window, cx),
            Some(Phase::Recording | Phase::Paused) => self.pause_voiceover(window, cx),
            Some(Phase::Ready | Phase::Attached) => self.play_voiceover_take(cx),
            _ => {}
        }
    }

    pub(super) fn play_voiceover_take(&mut self, cx: &mut Context<Self>) {
        let Some(panel) = &self.voiceover.panel else {
            return;
        };
        if panel.follow_picture || panel.replacement.is_some() || panel.phase == Phase::Attached {
            self.review_voiceover(cx);
        } else if let Some(take) = &panel.ready {
            self.toggle_audio_audition_with_gain(
                "voiceover-take",
                take.path(),
                panel.gain(take),
                cx,
            );
        }
    }

    fn review_voiceover(&mut self, cx: &mut Context<Self>) {
        if self.playing {
            self.stop_playback(cx);
            return;
        }
        self.library_audition = None;
        let Some(panel) = self.voiceover.panel.as_ref() else {
            return;
        };
        let Some(take) = panel.ready.as_ref() else {
            return;
        };
        let replacement = panel
            .replacement
            .as_ref()
            .map(|segment| segment.path.as_str());
        let Some((project, range)) = review_project(
            &self.project,
            take,
            replacement,
            panel.placement.lane,
            panel.gain(take),
        ) else {
            return;
        };
        if !self.play_voiceover_preview(
            range.start,
            PreviewPlayback::Review(Arc::new(project)),
            Some(range.end),
            cx,
        ) {
            if let Some(panel) = self.voiceover.panel.as_mut() {
                panel.error = Some(tr::editor::voiceover::picture_stopped().into());
            }
            cx.notify();
        }
    }

    pub(super) fn retake_voiceover(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.stop_playback(cx);
        self.library_audition = None;
        let Some(panel) = self.voiceover.panel.as_mut() else {
            return;
        };
        if panel.phase != Phase::Ready {
            return;
        }
        panel.ready = None;
        panel.phase = Phase::Idle;
        panel.error = None;
        self.start_voiceover(window, cx);
    }

    pub(super) fn pause_voiceover(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let total = self.total_duration();
        let Some(panel) = self.voiceover.panel.as_mut() else {
            return;
        };
        if panel.pause_pending || !matches!(panel.phase, Phase::Recording | Phase::Paused) {
            return;
        }
        if let Some(sync) = &panel.sync {
            let start = panel.placement.start;
            let sync = sync.clone();
            if panel.phase == Phase::Recording {
                let elapsed = sync.hold(None, Instant::now());
                panel.phase = Phase::Paused;
                self.park_voiceover_preview(start + elapsed.as_secs_f64(), cx);
            } else {
                let at = start + sync.elapsed(Instant::now()).as_secs_f64();
                let end = panel.range(total).end;
                if at >= end {
                    self.finish_voiceover(false, window, cx);
                } else if self.start_voiceover_picture(at, end, cx) {
                    self.voiceover.panel.as_mut().unwrap().phase = Phase::Recording;
                }
            }
            cx.notify();
            return;
        }
        let Some(session) = panel.session.clone() else {
            return;
        };
        let paused = panel.phase == Phase::Recording;
        let ticket = panel.ticket.clone();
        panel.pause_pending = true;
        cx.spawn_in(window, async move |this, cx: &mut AsyncWindowContext| {
            let result = gpui_tokio::Tokio::spawn(cx, async move {
                let session = session.lock().await;
                let session = session
                    .as_ref()
                    .ok_or_else(|| tr::editor::voiceover::recording_ended().to_string())?;
                if paused {
                    session.pause().await
                } else {
                    session.resume().await
                }
                .map_err(|error| format!("{error:#}"))
            })
            .await
            .unwrap_or_else(|error| Err(error.to_string()));
            let _ = this.update_in(cx, |this, _, cx| {
                if !this.voiceover_matches(&ticket) {
                    return;
                }
                let panel = this.voiceover.panel.as_mut().unwrap();
                panel.pause_pending = false;
                if matches!(panel.phase, Phase::Recording | Phase::Paused) {
                    match result {
                        Ok(()) => {
                            panel.phase = if paused {
                                Phase::Paused
                            } else {
                                Phase::Recording
                            }
                        }
                        Err(error) => panel.error = Some(error),
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    pub(super) fn finish_voiceover(
        &mut self,
        cancel: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(panel) = self.voiceover.panel.as_mut() else {
            return;
        };
        if panel.phase == Phase::Starting {
            if cancel {
                if let Some(signal) = &panel.startup_cancel {
                    let _ = signal.send(true);
                }
                panel.phase = Phase::Finalizing;
                cx.notify();
            }
            return;
        }
        if !matches!(
            panel.phase,
            Phase::Recording | Phase::Paused | Phase::SourceFinalizing
        ) {
            return;
        }
        let Some(session) = panel.session.take() else {
            return;
        };
        let elapsed = panel
            .sync
            .as_ref()
            .map(|sync| sync.hold(None, Instant::now()));
        let stopped_at = elapsed.map(|elapsed| panel.placement.start + elapsed.as_secs_f64());
        let cancel = cancel || elapsed == Some(Duration::ZERO);
        let ticket = panel.ticket.clone();
        panel.phase = Phase::Finalizing;
        panel.error = None;
        if let Some(time) = stopped_at {
            self.park_voiceover_preview(time, cx);
        }
        cx.spawn_in(window, async move |this, cx: &mut AsyncWindowContext| {
            let result = gpui_tokio::Tokio::spawn(cx, async move {
                let session = session
                    .lock()
                    .await
                    .take()
                    .ok_or_else(|| tr::editor::voiceover::recording_ended().to_string())?;
                if cancel {
                    session.cancel().await.map(|()| None)
                } else {
                    session.stop().await.map(Some)
                }
                .map_err(|error| format!("{error:#}"))
            })
            .await
            .unwrap_or_else(|error| Err(error.to_string()));
            let _ = this.update_in(cx, |this, window, cx| {
                if !this.voiceover_matches(&ticket) {
                    return;
                }
                let panel = this.voiceover.panel.as_mut().unwrap();
                match result {
                    Ok(Some(take)) => {
                        panel.ready = Some(take);
                        panel.phase = Phase::Ready;
                    }
                    Ok(None) => {
                        if panel.close_after_cancel {
                            this.voiceover.panel = None;
                            this.focus_root(window, cx);
                        } else {
                            panel.phase = Phase::Idle;
                            panel.status = None;
                        }
                    }
                    Err(error) => {
                        panel.phase = Phase::Error;
                        panel.error = Some(error);
                    }
                }
                this.reload_voiceover_takes(window, cx);
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
}
