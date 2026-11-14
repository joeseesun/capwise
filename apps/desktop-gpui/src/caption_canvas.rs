//! Editing a caption where it is drawn: double-click the caption on the
//! preview (or press Return with a caption selected) and type over it.
//!
//! The renderer keeps laying the caption out but skips drawing it while the
//! box is open, so the box sits exactly where the caption is without doubling
//! it. Every keystroke goes through the same live path as the sidebar field,
//! and one sentence is one undo step.
//!
//! Keys, matched before the field's own bindings and never while an input
//! method is composing:
//! - Tab / Shift-Tab: keep the sentence and move to the next / previous one.
//! - Return: play this sentence once and stop at its end, box still open.
//! - Cmd-Return: split the sentence at the caret.
//! - Up / Down: move between the caption and its translation.
//! - Escape: put the sentence back the way it was and close the box.
//!
//! The sidebar caption field gets Tab / Shift-Tab, Return and Cmd-Return too,
//! so the list and the preview behave alike.

use cap_project::{CaptionTrackSegment, CaptionsData, ProjectConfiguration};
use gpui::{
    AnyElement, AppContext, Context, CursorStyle, Entity, FontWeight, Hsla, InteractiveElement,
    IntoElement, Keystroke, MouseButton, MouseDownEvent, ParentElement, Pixels, Point,
    SharedString, Styled, Subscription, Window, div, px,
};

use crate::{
    editor_canvas::{NormRect, norm_rect},
    editor_edits::Selection,
    editor_panels::FieldKey,
    editor_timeline::TrackKind,
    editor_window::EditorWindow,
    i18n::tr,
    ui,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CaptionLine {
    Source,
    Translation,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CaptionKey {
    Next,
    Previous,
    Audition,
    Split,
    Up,
    Down,
    Cancel,
}

fn caption_key(keystroke: &Keystroke) -> Option<CaptionKey> {
    let modifiers = keystroke.modifiers;
    if modifiers.control || modifiers.alt || modifiers.function {
        return None;
    }
    Some(
        match (keystroke.key.as_str(), modifiers.platform, modifiers.shift) {
            ("tab", false, false) => CaptionKey::Next,
            ("tab", false, true) => CaptionKey::Previous,
            ("enter", false, false) => CaptionKey::Audition,
            ("enter", true, false) => CaptionKey::Split,
            ("up", false, false) => CaptionKey::Up,
            ("down", false, false) => CaptionKey::Down,
            ("escape", false, false) => CaptionKey::Cancel,
            _ => return None,
        },
    )
}

struct CaptionSnapshot {
    captions: Option<CaptionsData>,
    track: Vec<CaptionTrackSegment>,
}

impl CaptionSnapshot {
    fn take(project: &ProjectConfiguration) -> Self {
        Self {
            captions: project.captions.clone(),
            track: project
                .timeline
                .as_ref()
                .map(|timeline| timeline.caption_segments.clone())
                .unwrap_or_default(),
        }
    }

    fn restore(self, project: &mut ProjectConfiguration) {
        project.captions = self.captions;
        if let Some(timeline) = project.timeline.as_mut() {
            timeline.caption_segments = self.track;
        }
    }
}

struct TranslationLine {
    input: Entity<ui::TextInputState>,
    language: String,
}

pub(crate) struct CaptionCanvasEdit {
    id: String,
    line: CaptionLine,
    source: Entity<ui::TextInputState>,
    translation: Option<TranslationLine>,
    before: Option<CaptionSnapshot>,
    auditioning: bool,
    _events: Vec<Subscription>,
}

impl CaptionCanvasEdit {
    fn input(&self, line: CaptionLine) -> Option<&Entity<ui::TextInputState>> {
        match line {
            CaptionLine::Source => Some(&self.source),
            CaptionLine::Translation => self.translation.as_ref().map(|line| &line.input),
        }
    }

    fn focused_line(&self, window: &Window, cx: &gpui::App) -> Option<CaptionLine> {
        [CaptionLine::Source, CaptionLine::Translation]
            .into_iter()
            .find(|line| {
                self.input(*line)
                    .is_some_and(|input| input.read(cx).is_focused(window))
            })
    }
}

/// The time a caption is fully on screen: its middle, past any fade-in.
fn caption_anchor(segment: &CaptionTrackSegment) -> f64 {
    segment.start + (segment.end - segment.start).max(0.) / 2.
}

/// Byte offset of the word boundary at or before `caret`, and the output
/// time the split falls on, from the caption's word timings when it has
/// them and its length otherwise.
fn split_point(segment: &CaptionTrackSegment, caret: usize) -> Option<(usize, f64)> {
    let text = segment.text.as_str();
    let caret = caret.min(text.len());
    if !segment.words.is_empty() {
        let mut from = 0;
        let mut spans = Vec::with_capacity(segment.words.len());
        for word in &segment.words {
            let word_text = word.text.trim();
            let start = from + text.get(from..)?.find(word_text)?;
            from = start + word_text.len();
            spans.push(start..from);
        }
        let before = spans.partition_point(|span| span.end <= caret);
        if before == 0 || before == spans.len() {
            return None;
        }
        let at = (f64::from(segment.words[before - 1].end)
            + f64::from(segment.words[before].start))
            / 2.;
        return Some((spans[before].start, at));
    }
    let head = text.get(..caret)?.trim();
    let tail = text.get(caret..)?.trim();
    if head.is_empty() || tail.is_empty() {
        return None;
    }
    let total = text.chars().filter(|ch| !ch.is_whitespace()).count().max(1) as f64;
    let before = head.chars().filter(|ch| !ch.is_whitespace()).count() as f64;
    Some((
        caret,
        segment.start + (segment.end - segment.start) * before / total,
    ))
}

fn hex(color: &str) -> Option<Hsla> {
    u32::from_str_radix(color.trim().trim_start_matches('#'), 16)
        .ok()
        .map(|value| Hsla::from(gpui::rgb(value)))
}

fn caption_font_family(font: &str) -> Option<SharedString> {
    match font {
        "System Sans-Serif" | "System Monospace" | "" => None,
        "System Serif" if cfg!(target_os = "macos") => Some("Songti SC".into()),
        "System Serif" => Some("serif".into()),
        "LXGW WenKai Lite Medium" => Some("LXGW WenKai Lite".into()),
        family => Some(SharedString::from(family.to_owned())),
    }
}

/// Routes the caption keys of this window to the caption being edited, on
/// the preview or in the sidebar list.
pub(crate) fn install_caption_keys(
    window: &Window,
    cx: &mut Context<EditorWindow>,
) -> Subscription {
    let owner = window.window_handle().window_id();
    let editor = cx.weak_entity();
    cx.intercept_keystrokes(move |event, window, cx| {
        if window.window_handle().window_id() != owner {
            return;
        }
        let Some(key) = caption_key(&event.keystroke) else {
            return;
        };
        if ui::text_input::text_input_is_composing(window, cx) {
            return;
        }
        let Some(editor) = editor.upgrade() else {
            return;
        };
        let handled = editor.update(cx, |this, cx| this.caption_key(key, window, cx));
        if handled {
            window.prevent_default();
            cx.stop_propagation();
        }
    })
}

impl EditorWindow {
    pub(crate) fn caption_edit_open(&self) -> bool {
        self.caption_edit.is_some()
    }

    /// Whether the preview should leave the caption to the edit box.
    pub(crate) fn caption_edit_hides_caption(&self) -> bool {
        self.caption_edit
            .as_ref()
            .is_some_and(|edit| !edit.auditioning)
    }

    fn caption_segments(&self) -> &[CaptionTrackSegment] {
        self.project
            .timeline
            .as_ref()
            .map(|timeline| timeline.caption_segments.as_slice())
            .unwrap_or_default()
    }

    fn caption_index(&self, id: &str) -> Option<usize> {
        self.caption_segments()
            .iter()
            .position(|segment| segment.id == id)
    }

    /// The caption on screen at `time`: the one running then, or the last one
    /// still lingering after its end.
    pub(crate) fn caption_index_at(&self, time: f64) -> Option<usize> {
        let segments = self.caption_segments();
        if let Some(index) = segments
            .iter()
            .position(|segment| time >= segment.start && time < segment.end)
        {
            return Some(index);
        }
        let linger = self.project.captions.as_ref().map_or(0., |captions| {
            f64::from(captions.settings.linger_duration.max(0.))
        });
        segments
            .iter()
            .enumerate()
            .filter(|(_, segment)| segment.start <= time && time < segment.end + linger)
            .max_by(|(_, a), (_, b)| a.start.total_cmp(&b.start))
            .map(|(index, _)| index)
    }

    /// The first moment `segment` is drawn at full strength, past its fade.
    fn caption_visible_from(&self, segment: &CaptionTrackSegment) -> f64 {
        let fade = segment.fade_duration_override.unwrap_or_else(|| {
            self.project
                .captions
                .as_ref()
                .map_or(0., |captions| captions.settings.fade_duration)
        });
        segment.start + f64::from(fade.max(0.)).min((segment.end - segment.start).max(0.) / 2.)
    }

    /// The translation language a caption is edited in: its own, or the one
    /// the rest of the captions were translated into.
    fn caption_translation_language(&self, id: &str) -> Option<String> {
        let settings = &self.project.captions.as_ref()?.settings;
        if settings.display_mode == cap_project::CaptionDisplayMode::Original {
            return None;
        }
        [id, cap_project::source_caption_id(id)]
            .into_iter()
            .find_map(|key| settings.translations.get(key))
            .or_else(|| settings.translations.values().next())
            .map(|translation| translation.language.clone())
            .filter(|language| !language.trim().is_empty())
    }

    fn caption_translation_text(&self, id: &str) -> Option<String> {
        let settings = &self.project.captions.as_ref()?.settings;
        [id, cap_project::source_caption_id(id)]
            .into_iter()
            .find_map(|key| settings.translations.get(key))
            .map(|translation| translation.text.clone())
    }

    /// Opens the box on caption `index`. `line` picks the line the caret
    /// starts in; `None` takes the one the viewer reads.
    pub(crate) fn begin_caption_edit(
        &mut self,
        index: usize,
        line: Option<CaptionLine>,
        caret_at_start: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.project_ready() || self.voiceover.is_busy() {
            return;
        }
        self.finish_caption_edit(window, cx);
        let Some(segment) = self.caption_segments().get(index).cloned() else {
            return;
        };
        self.stop_canvas_text_edit(cx);
        self.end_field_edit(cx);
        if self.playing {
            self.stop_playback(cx);
        }
        let playhead = self.playhead_time();
        if self.caption_index_at(playhead) != Some(index)
            || playhead < self.caption_visible_from(&segment)
        {
            self.seek_to_time(caption_anchor(&segment), cx);
        }
        self.set_selection_with_scroll(
            Some(Selection::single(TrackKind::Caption, index)),
            true,
            cx,
        );
        self.reveal_timeline_range(segment.start, segment.end, cx);

        let source = cx.new(|cx| ui::TextInputState::multi_line(window, cx));
        source.update(cx, |input, cx| input.set_text(segment.text.clone(), cx));
        let translation = self
            .caption_translation_language(&segment.id)
            .map(|language| {
                let input = cx.new(|cx| ui::TextInputState::multi_line(window, cx));
                let text = self
                    .caption_translation_text(&segment.id)
                    .unwrap_or_default();
                input.update(cx, |input, cx| {
                    input.set_placeholder(tr::editor::canvas::caption_translation());
                    input.set_text(text, cx);
                });
                TranslationLine { input, language }
            });
        let translated_view = self.project.captions.as_ref().is_some_and(|captions| {
            captions.settings.display_mode == cap_project::CaptionDisplayMode::Translated
        });
        let line = match line {
            Some(CaptionLine::Translation) | None
                if translation.is_some() && (line.is_some() || translated_view) =>
            {
                CaptionLine::Translation
            }
            _ => CaptionLine::Source,
        };

        let mut events = Vec::new();
        for (input, input_line) in [(Some(&source), CaptionLine::Source)].into_iter().chain([(
            translation.as_ref().map(|line| &line.input),
            CaptionLine::Translation,
        )]) {
            let Some(input) = input else {
                continue;
            };
            let id = segment.id.clone();
            events.push(cx.subscribe_in(
                input,
                window,
                move |this: &mut Self, input, event: &ui::TextInputEvent, window, cx| {
                    if this.caption_edit.as_ref().is_none_or(|edit| edit.id != id) {
                        return;
                    }
                    match event {
                        ui::TextInputEvent::Changed => {
                            let text = input.read(cx).text().to_owned();
                            this.caption_edit_typed(input_line, text, window, cx);
                        }
                        ui::TextInputEvent::Cancelled => this.cancel_caption_edit(window, cx),
                        ui::TextInputEvent::Confirmed => {}
                        ui::TextInputEvent::Blurred => {
                            let id = id.clone();
                            cx.defer_in(window, move |this, window, cx| {
                                let Some(edit) = this.caption_edit.as_ref() else {
                                    return;
                                };
                                if edit.id == id && edit.focused_line(window, cx).is_none() {
                                    this.finish_caption_edit(window, cx);
                                }
                            });
                        }
                    }
                },
            ));
        }

        self.history.pause();
        self.caption_edit = Some(CaptionCanvasEdit {
            id: segment.id.clone(),
            line,
            source,
            translation,
            before: Some(CaptionSnapshot::take(&self.project)),
            auditioning: false,
            _events: events,
        });
        self.focus_caption_line(line, caret_at_start, window, cx);
        self.publish_project();
        tracing::info!(index, ?line, "caption canvas edit");
        cx.notify();
    }

    fn focus_caption_line(
        &mut self,
        line: CaptionLine,
        caret_at_start: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(edit) = self.caption_edit.as_mut() else {
            return;
        };
        let Some(input) = edit.input(line).cloned() else {
            return;
        };
        edit.line = line;
        input.update(cx, |input, cx| {
            if caret_at_start {
                input.place_cursor(0, cx);
            } else {
                input.move_to_end(cx);
            }
        });
        let handle = input.read(cx).focus_handle();
        window.focus(&handle, cx);
    }

    fn caption_edit_typed(
        &mut self,
        line: CaptionLine,
        text: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(edit) = self.caption_edit.as_mut() else {
            return;
        };
        edit.line = line;
        let id = edit.id.clone();
        if edit.auditioning {
            edit.auditioning = false;
            self.stop_playback(cx);
            if let Some(segment) = self
                .caption_index(&id)
                .and_then(|index| self.caption_segments().get(index))
            {
                let anchor = caption_anchor(segment);
                self.seek_to_time(anchor, cx);
            }
        }
        let Some(index) = self.caption_index(&id) else {
            return;
        };
        let changed = match line {
            CaptionLine::Source => self.edit_caption_text_live(index, text),
            CaptionLine::Translation => self.write_caption_translation(index, &text),
        };
        if changed {
            self.schedule_save(window, cx);
            self.invalidate_caption_views(cx);
        } else {
            self.publish_project();
        }
        cx.notify();
    }

    fn write_caption_translation(&mut self, index: usize, text: &str) -> bool {
        if !self.project_ready() || self.voiceover.is_busy() {
            return false;
        }
        let Some(language) = self
            .caption_edit
            .as_ref()
            .and_then(|edit| edit.translation.as_ref())
            .map(|line| line.language.clone())
        else {
            return false;
        };
        let Some((captions, timeline)) = self
            .project
            .captions
            .as_mut()
            .zip(self.project.timeline.as_ref())
        else {
            return false;
        };
        let Some(segment) = timeline.caption_segments.get(index) else {
            return false;
        };
        let settings = &mut captions.settings;
        let source_text = crate::caption_translation::translation_source_text(segment, settings);
        let id = segment.id.as_str();
        if text.trim().is_empty() {
            let removed = settings.translations.remove(id).is_some()
                | settings
                    .translations
                    .remove(cap_project::source_caption_id(id))
                    .is_some();
            if !removed {
                return false;
            }
        } else {
            if settings
                .translations
                .get(id)
                .is_some_and(|current| current.text == text && current.source_text == source_text)
            {
                return false;
            }
            settings.set_translation(id, &source_text, text.to_owned(), language);
        }
        self.history.record(&self.project);
        self.publish_project();
        true
    }

    /// Closes the box and keeps what was typed, as one undo step.
    pub(crate) fn finish_caption_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(edit) = self.caption_edit.take() else {
            return;
        };
        let focused = edit.focused_line(window, cx).is_some();
        if edit.auditioning && self.playing {
            self.stop_playback(cx);
        }
        drop(edit);
        self.history.resume(&self.project);
        self.queue_caption_translation_update(window, cx);
        self.note_project_edit("caption-canvas");
        self.publish_project();
        self.invalidate_caption_views(cx);
        if focused {
            self.focus_root(window, cx);
        }
        cx.notify();
    }

    /// Escape: the sentence goes back to how it was when the box opened.
    pub(crate) fn cancel_caption_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(mut edit) = self.caption_edit.take() else {
            return;
        };
        if edit.auditioning && self.playing {
            self.stop_playback(cx);
        }
        if let Some(before) = edit.before.take() {
            before.restore(&mut self.project);
            self.history.record(&self.project);
            self.caption_track_replaced();
            self.schedule_save(window, cx);
        }
        drop(edit);
        self.history.resume(&self.project);
        self.publish_project();
        self.invalidate_caption_views(cx);
        self.focus_root(window, cx);
        cx.notify();
    }

    fn step_caption_edit(&mut self, forward: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(edit) = self.caption_edit.as_ref() else {
            return;
        };
        let line = edit.line;
        let Some(index) = self.caption_index(&edit.id) else {
            return;
        };
        let next = if forward {
            index + 1
        } else {
            match index.checked_sub(1) {
                Some(index) => index,
                None => return,
            }
        };
        if next >= self.caption_segments().len() {
            return;
        }
        self.begin_caption_edit(next, Some(line), false, window, cx);
    }

    fn audition_caption_edit(&mut self, cx: &mut Context<Self>) {
        let Some(edit) = self.caption_edit.as_mut() else {
            return;
        };
        let id = edit.id.clone();
        let Some(segment) = self
            .caption_index(&id)
            .and_then(|index| self.caption_segments().get(index))
        else {
            return;
        };
        let (start, end) = (segment.start, segment.end);
        if let Some(edit) = self.caption_edit.as_mut() {
            edit.auditioning = true;
        }
        self.publish_project();
        self.play_range(start, end, cx);
    }

    /// The playback a Return started reached the sentence's end: show the
    /// sentence again with the box over it.
    pub(crate) fn caption_audition_ended(&mut self, cx: &mut Context<Self>) {
        let Some(edit) = self.caption_edit.as_mut() else {
            return;
        };
        if !edit.auditioning {
            return;
        }
        edit.auditioning = false;
        let id = edit.id.clone();
        if let Some(anchor) = self
            .caption_index(&id)
            .and_then(|index| self.caption_segments().get(index))
            .map(caption_anchor)
        {
            self.seek_to_time(anchor, cx);
        }
        self.publish_project();
        cx.notify();
    }

    fn split_caption_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(edit) = self.caption_edit.as_ref() else {
            return;
        };
        if edit.line != CaptionLine::Source {
            return;
        }
        let caret = edit.source.read(cx).selection().start;
        let Some(index) = self.caption_index(&edit.id) else {
            return;
        };
        if self
            .caption_segments()
            .get(index)
            .and_then(|segment| split_point(segment, caret))
            .is_none()
        {
            return;
        }
        self.finish_caption_edit(window, cx);
        if self.split_caption_at(index, caret, window, cx) {
            self.begin_caption_edit(index + 1, Some(CaptionLine::Source), true, window, cx);
        }
    }

    /// Splits caption `index` at byte `caret` of its text, as one undo step.
    /// The second half keeps the first's translation as out of date, so both
    /// halves are translated again.
    fn split_caption_at(
        &mut self,
        index: usize,
        caret: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(segment) = self.caption_segments().get(index).cloned() else {
            return false;
        };
        let Some((byte, at)) = split_point(&segment, caret) else {
            return false;
        };
        let durations = self.clip_display_durations.clone();
        let translation = self.project.captions.as_ref().and_then(|captions| {
            [
                segment.id.as_str(),
                cap_project::source_caption_id(&segment.id),
            ]
            .into_iter()
            .find_map(|key| captions.settings.translations.get(key))
            .cloned()
        });
        self.edit_caption_content("caption-split", window, cx, |project| {
            if !crate::editor_window::split_caption_segment(
                project,
                index,
                at - segment.start,
                &durations,
            ) {
                return false;
            }
            let Some(tail_id) = project
                .timeline
                .as_ref()
                .and_then(|timeline| timeline.caption_segments.get(index + 1))
                .map(|tail| tail.id.clone())
            else {
                return true;
            };
            if segment.words.is_empty() {
                if let Some(timeline) = project.timeline.as_mut() {
                    timeline.caption_segments[index].text = segment.text[..byte].trim().to_owned();
                    timeline.caption_segments[index + 1].text =
                        segment.text[byte..].trim().to_owned();
                }
                crate::transcription::write_caption_edit_to_source(project, index, &durations);
                crate::transcription::write_caption_edit_to_source(project, index + 1, &durations);
            }
            if let (Some(translation), Some(captions)) = (translation, project.captions.as_mut()) {
                captions.settings.translations.insert(tail_id, translation);
            }
            true
        })
    }

    /// Moves the sidebar's caption field to caption `index`, caret at the end
    /// or the start.
    fn focus_caption_field(
        &mut self,
        index: usize,
        caret_at_start: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(segment) = self.caption_segments().get(index).cloned() else {
            return;
        };
        let field = FieldKey::CaptionText(index);
        self.ensure_field(field, window, cx);
        let Some(input) = self.field(field).cloned() else {
            return;
        };
        input.update(cx, |input, cx| {
            input.set_text(segment.text.clone(), cx);
            if caret_at_start {
                input.place_cursor(0, cx);
            } else {
                input.move_to_end(cx);
            }
        });
        self.set_selection_with_scroll(
            Some(Selection::single(TrackKind::Caption, index)),
            true,
            cx,
        );
        self.seek_to_time(caption_anchor(&segment), cx);
        self.reveal_timeline_range(segment.start, segment.end, cx);
        let handle = input.read(cx).focus_handle();
        window.focus(&handle, cx);
        self.invalidate_caption_views(cx);
    }

    /// Keeps the open box in step with the project between keystrokes: a
    /// translation that arrived, a caption that is gone.
    pub(crate) fn sync_caption_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(edit) = self.caption_edit.as_ref() else {
            return;
        };
        let Some(index) = self.caption_index(&edit.id) else {
            self.caption_edit = None;
            self.history.resume(&self.project);
            self.publish_project();
            return;
        };
        let focused = edit.focused_line(window, cx);
        let source_text = self.caption_segments()[index].text.clone();
        let source = edit.source.clone();
        if !source.read(cx).is_focused(window) && source.read(cx).text() != source_text {
            source.update(cx, |input, cx| input.set_text(source_text, cx));
        }
        if let Some(line) = edit.translation.as_ref() {
            let input = line.input.clone();
            let id = edit.id.clone();
            if !input.read(cx).is_focused(window)
                && let Some(text) = self.caption_translation_text(&id)
                && input.read(cx).text() != text
            {
                input.update(cx, |input, cx| input.set_text(text, cx));
            }
        }
        if let (Some(line), Some(edit)) = (focused, self.caption_edit.as_mut()) {
            edit.line = line;
        }
    }

    fn caption_key(
        &mut self,
        key: CaptionKey,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if let Some((line, auditioning)) = self
            .caption_edit
            .as_ref()
            .and_then(|edit| Some((edit.focused_line(window, cx)?, edit.auditioning)))
        {
            if auditioning && !self.playing {
                self.caption_audition_ended(cx);
            }
            match key {
                CaptionKey::Next => self.step_caption_edit(true, window, cx),
                CaptionKey::Previous => self.step_caption_edit(false, window, cx),
                CaptionKey::Audition => self.audition_caption_edit(cx),
                CaptionKey::Split => self.split_caption_edit(window, cx),
                CaptionKey::Cancel => {
                    if self
                        .caption_edit
                        .as_ref()
                        .is_some_and(|edit| edit.auditioning)
                    {
                        self.stop_playback(cx);
                        self.caption_audition_ended(cx);
                    } else {
                        self.cancel_caption_edit(window, cx);
                    }
                }
                CaptionKey::Up | CaptionKey::Down => {
                    let target = if key == CaptionKey::Down {
                        CaptionLine::Translation
                    } else {
                        CaptionLine::Source
                    };
                    let Some(edit) = self.caption_edit.as_ref() else {
                        return false;
                    };
                    let Some(input) = edit.input(line).cloned() else {
                        return false;
                    };
                    if line == target || edit.input(target).is_none() {
                        return false;
                    }
                    if !caret_on_edge_row(&input, key == CaptionKey::Down, cx) {
                        return false;
                    }
                    self.focus_caption_line(target, false, window, cx);
                }
            }
            window.refresh();
            return true;
        }
        self.caption_field_key(key, window, cx)
    }

    /// The same keys in the sidebar's caption field.
    fn caption_field_key(
        &mut self,
        key: CaptionKey,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(FieldKey::CaptionText(index)) = self.field_editing.or_else(|| {
            self.fields
                .iter()
                .find(|(key, input)| {
                    matches!(key, FieldKey::CaptionText(_)) && input.read(cx).is_focused(window)
                })
                .map(|(key, _)| *key)
        }) else {
            return false;
        };
        let Some(input) = self.field(FieldKey::CaptionText(index)).cloned() else {
            return false;
        };
        if !input.read(cx).is_focused(window) {
            return false;
        }
        match key {
            CaptionKey::Next | CaptionKey::Previous => {
                let next = if key == CaptionKey::Next {
                    index + 1
                } else if let Some(previous) = index.checked_sub(1) {
                    previous
                } else {
                    return true;
                };
                if next < self.caption_segments().len() {
                    self.focus_caption_field(next, false, window, cx);
                }
            }
            CaptionKey::Audition => {
                if let Some(segment) = self.caption_segments().get(index) {
                    let (start, end) = (segment.start, segment.end);
                    self.play_range(start, end, cx);
                }
            }
            CaptionKey::Split => {
                let caret = input.read(cx).selection().start;
                if self
                    .caption_segments()
                    .get(index)
                    .and_then(|segment| split_point(segment, caret))
                    .is_none()
                {
                    return true;
                }
                self.focus_root(window, cx);
                self.end_field_edit(cx);
                if self.split_caption_at(index, caret, window, cx) {
                    if let Some(head) = self.caption_segments().get(index) {
                        let head = head.text.clone();
                        input.update(cx, |input, cx| input.set_text(head, cx));
                    }
                    self.focus_caption_field(index + 1, true, window, cx);
                }
            }
            CaptionKey::Up | CaptionKey::Down | CaptionKey::Cancel => return false,
        }
        window.refresh();
        true
    }

    /// Return with one caption selected opens it on the preview.
    pub(crate) fn edit_selected_caption(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(index) = self
            .selection
            .as_ref()
            .filter(|selection| {
                selection.track == TrackKind::Caption && selection.indices.len() == 1
            })
            .map(|selection| selection.indices[0])
        else {
            return false;
        };
        self.begin_caption_edit(index, None, false, window, cx);
        self.caption_edit.is_some()
    }

    /// The caption box on the preview, in normalised frame space, for any
    /// placement.
    pub(crate) fn caption_drawn_rect(&self) -> Option<NormRect> {
        let layout = self.frame_layout.as_ref()?;
        let caption = layout.caption?;
        let rect = norm_rect(caption.bounds, layout);
        (rect.w > 0. && rect.h > 0.).then_some(rect)
    }

    /// A double-click on the caption while the video plays: stop there and
    /// edit the caption that was showing.
    pub(crate) fn caption_double_click_while_playing(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if event.click_count != 2 || !self.playing {
            return false;
        }
        let (Some(rect), Some(frame)) = (self.caption_drawn_rect(), self.canvas_bounds()) else {
            return false;
        };
        if !rect_contains(rect, frame, event.position) {
            return false;
        }
        let Some(index) = self.caption_index_at(self.playhead_time()) else {
            return false;
        };
        self.stop_playback(cx);
        self.begin_caption_edit(index, None, false, window, cx);
        true
    }

    pub(crate) fn render_caption_edit(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let edit = self.caption_edit.as_ref()?;
        let player = self.player_frame_rect.get()?;
        let captions = self.project.captions.as_ref()?;
        let settings = &captions.settings;
        let index = self.caption_index(&edit.id)?;
        let segment = self.caption_segments().get(index)?;
        let (cw, ch) = (
            f32::from(player.frame.size.width),
            f32::from(player.frame.size.height),
        );
        let layout = self.frame_layout.as_ref();
        let scale = layout.map_or(ch / 1080., |layout| {
            ch / layout.output_size[1].max(1) as f32
        });
        let font_size = layout
            .and_then(|layout| layout.caption)
            .map(|caption| caption.font_size)
            .filter(|size| *size > 0.)
            .map_or(settings.size as f32 * ch / 1080., |size| size * scale)
            .max(12.);
        let rect = self.caption_drawn_rect().unwrap_or(NormRect {
            x: 0.1,
            y: 0.78,
            w: 0.8,
            h: 0.12,
        });
        let pad = (font_size * 0.4).max(6.);
        let width = (rect.w as f32 * cw).max(font_size * 6.).min(cw);
        let left = (rect.x as f32 * cw + rect.w as f32 * cw / 2. - width / 2.)
            .clamp(0., (cw - width).max(0.));
        let bottom_anchored = rect.y + rect.h / 2. > 0.5;
        let theme = self.theme;
        let readable = |color: Option<Hsla>| {
            color
                .filter(|color| color.l >= 0.35)
                .unwrap_or_else(gpui::white)
        };
        let source_color = readable(hex(&settings.color));
        let family = caption_font_family(&settings.font);
        let weight = FontWeight(settings.font_weight.clamp(100, 900) as f32);
        let source_text = crate::caption_translation::translation_source_text(segment, settings);
        let stale = settings
            .translation_for(&segment.id, &source_text)
            .is_none();

        let line_input =
            |id: &'static str, input: &Entity<ui::TextInputState>, size: f32, color: Hsla| {
                ui::TextInput::bare(&theme, id, input)
                    .padding_x(px(0.))
                    .padding_y(px(0.))
                    .text_size(px(size))
                    .line_height(px(size * 1.3))
                    .font_weight(weight)
                    .align(gpui::TextAlign::Center)
                    .text_color(color)
                    .caret_color(gpui::white())
                    .selection_color(gpui::hsla(0.58, 0.9, 0.6, 0.4))
            };

        let mut column = div()
            .flex()
            .flex_col()
            .gap(px(font_size * 0.2))
            .child(line_input(
                "caption-canvas-source",
                &edit.source,
                font_size,
                source_color,
            ));
        if let Some(line) = edit.translation.as_ref() {
            let appearance = settings.resolved_translation_appearance();
            let color = readable(hex(&appearance.color));
            let color = if stale && !line.input.read(cx).text().is_empty() {
                color.opacity(0.45)
            } else {
                color
            };
            column = column.child(line_input(
                "caption-canvas-translation",
                &line.input,
                font_size * 0.8,
                color,
            ));
        }

        let origin_x = player.frame.origin.x - player.container.origin.x;
        let origin_y = player.frame.origin.y - player.container.origin.y;
        let mut panel = div()
            .id("caption-canvas-edit")
            .absolute()
            .left(px(left))
            .w(px(width))
            .px(px(pad))
            .py(px(pad * 0.6))
            .rounded(px(font_size * 0.35))
            .bg(gpui::hsla(0., 0., 0., 0.62))
            .border_1()
            .border_color(Hsla::from(theme.blue_9))
            .cursor(CursorStyle::IBeam)
            .occlude()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _: &MouseDownEvent, window, cx| {
                    cx.stop_propagation();
                    let Some(edit) = this.caption_edit.as_ref() else {
                        return;
                    };
                    if edit.focused_line(window, cx).is_none() {
                        let line = edit.line;
                        this.focus_caption_line(line, false, window, cx);
                    }
                }),
            )
            .child(column);
        if let Some(family) = family {
            panel = panel.font_family(family);
        }
        panel = if bottom_anchored {
            panel.bottom(px(((1. - rect.y - rect.h) as f32 * ch).max(0.)))
        } else {
            panel.top(px((rect.y as f32 * ch).max(0.)))
        };
        if edit.auditioning {
            panel = panel.opacity(0.);
        }
        Some(
            div()
                .absolute()
                .left(origin_x)
                .top(origin_y)
                .w(player.frame.size.width)
                .h(player.frame.size.height)
                .child(panel)
                .into_any_element(),
        )
    }
}

fn caret_on_edge_row(input: &Entity<ui::TextInputState>, last: bool, cx: &gpui::App) -> bool {
    let input = input.read(cx);
    let caret = input.selection().end;
    let edge = if last { input.text().len() } else { 0 };
    match (
        input.visual_row_bounds_for_offset(caret),
        input.visual_row_bounds_for_offset(edge),
    ) {
        (Some(caret), Some(edge)) => caret == edge,
        _ => true,
    }
}

fn rect_contains(rect: NormRect, frame: gpui::Bounds<Pixels>, point: Point<Pixels>) -> bool {
    let x = f64::from(f32::from(point.x - frame.origin.x) / f32::from(frame.size.width).max(1.));
    let y = f64::from(f32::from(point.y - frame.origin.y) / f32::from(frame.size.height).max(1.));
    x >= rect.x && x <= rect.x + rect.w && y >= rect.y && y <= rect.y + rect.h
}

#[cfg(test)]
mod tests {
    use super::*;

    fn segment(text: &str, words: &[(&str, f32, f32)]) -> CaptionTrackSegment {
        serde_json::from_value(serde_json::json!({
            "id": "a",
            "start": 10.0,
            "end": 14.0,
            "text": text,
            "words": words
                .iter()
                .map(|(text, start, end)| serde_json::json!({"text": text, "start": start, "end": end}))
                .collect::<Vec<_>>(),
        }))
        .unwrap()
    }

    fn split(caption: &CaptionTrackSegment, caret: usize) -> Option<(usize, f64)> {
        split_point(caption, caret).map(|(byte, at)| (byte, (at * 1000.).round() / 1000.))
    }

    #[test]
    fn a_split_lands_between_the_words_around_the_caret() {
        let caption = segment(
            "open the app now",
            &[
                ("open", 10.0, 10.4),
                ("the", 10.5, 10.7),
                ("app", 11.0, 11.5),
                ("now", 12.0, 12.4),
            ],
        );
        assert_eq!(split(&caption, 9), Some((9, 10.85)));
        assert_eq!(split(&caption, 10), Some((9, 10.85)));
        assert_eq!(split(&caption, 13), Some((13, 11.75)));
        assert_eq!(split(&caption, 0), None);
        assert_eq!(split(&caption, caption.text.len()), None);
    }

    #[test]
    fn chinese_words_split_at_the_character_the_caret_is_on() {
        let caption = segment(
            "打开设置页面",
            &[
                ("打开", 10.0, 10.6),
                ("设置", 10.8, 11.4),
                ("页面", 11.6, 12.0),
            ],
        );
        assert_eq!(
            split(&caption, "打开".len()),
            Some(("打开".len(), 10.7))
        );
        assert_eq!(
            split(&caption, "打开设".len()),
            Some(("打开".len(), 10.7))
        );
    }

    #[test]
    fn a_caption_without_word_times_splits_by_length() {
        let caption = segment("一二 三四", &[]);
        let (byte, at) = split_point(&caption, "一二".len()).unwrap();
        assert_eq!(byte, "一二".len());
        assert!((at - 12.0).abs() < 1e-9);
        assert_eq!(split_point(&caption, 0), None);
    }

    #[test]
    fn caption_keys_leave_other_chords_to_the_field() {
        let key = |text: &str| caption_key(&Keystroke::parse(text).unwrap());
        assert_eq!(key("tab"), Some(CaptionKey::Next));
        assert_eq!(key("shift-tab"), Some(CaptionKey::Previous));
        assert_eq!(key("enter"), Some(CaptionKey::Audition));
        assert_eq!(key("cmd-enter"), Some(CaptionKey::Split));
        assert_eq!(key("escape"), Some(CaptionKey::Cancel));
        assert_eq!(key("shift-enter"), None);
        assert_eq!(key("cmd-z"), None);
        assert_eq!(key("alt-up"), None);
        assert_eq!(key("a"), None);
    }
}
