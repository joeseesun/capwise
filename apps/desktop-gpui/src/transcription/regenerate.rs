use std::{ops::Range, path::Path, sync::atomic::AtomicBool};

use cap_project::{CaptionRecognition, CaptionSegment, CaptionWord, ProjectConfiguration};

use super::*;

const MAX_REFERENCE_SECONDS: f32 = 8.0;
const MIN_ALIGNMENT_COVERAGE: f32 = 0.62;

pub(crate) fn recognition(engine: &str, language: &str) -> CaptionRecognition {
    CaptionRecognition {
        engine: engine.into(),
        model: crate::asr_cloud::service(engine)
            .map(|service| service.endpoint().model)
            .unwrap_or_else(|| engine.into()),
        language: language.into(),
    }
}

fn spoken_characters(text: &str) -> impl Iterator<Item = char> + '_ {
    text.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
}

fn overlaps(start: f64, end: f64, range: &Range<f64>) -> bool {
    end.min(range.end) - start.max(range.start) > 0.005
}

pub(crate) fn suspicious_ranges(
    project: &ProjectConfiguration,
    visible: &[Range<f64>],
) -> Vec<Range<f64>> {
    let Some(captions) = project
        .captions
        .as_ref()
        .filter(|captions| captions.source_timed)
    else {
        return Vec::new();
    };
    let mut runs = Vec::<(Range<f64>, usize)>::new();
    for segment in &captions.segments {
        let start = f64::from(segment.start);
        let end = f64::from(segment.end);
        let length = spoken_characters(&segment.text).count();
        if segment.manual_timing.is_some() || !(0.0..0.35).contains(&(end - start)) || length < 5 {
            continue;
        }
        if let Some((run, count)) = runs.last_mut()
            && start >= run.start
            && start <= run.end + 0.15
        {
            run.end = run.end.max(end);
            *count += 1;
        } else {
            runs.push((start..end, 1));
        }
    }
    let mut result = Vec::new();
    for (run, _) in runs.into_iter().filter(|(_, count)| *count >= 3) {
        for clip in visible {
            if overlaps(run.start, run.end, clip) {
                result.push(run.start.max(clip.start)..run.end.min(clip.end));
            }
        }
    }
    result
}

struct Anchor {
    character: char,
    start: f32,
    end: f32,
}

fn reference_anchors(groups: &[Vec<CaptionWord>]) -> Vec<Anchor> {
    let mut anchors = Vec::new();
    let mut last = 0.0;
    for word in groups.iter().flatten() {
        let characters: Vec<_> = spoken_characters(&word.text).collect();
        let start = word.start.max(last);
        if !word.end.is_finite() || word.end <= start || characters.is_empty() {
            continue;
        }
        let step = (word.end - start) / characters.len() as f32;
        anchors.extend(
            characters
                .into_iter()
                .enumerate()
                .map(|(i, character)| Anchor {
                    character,
                    start: start + i as f32 * step,
                    end: start + (i + 1) as f32 * step,
                }),
        );
        last = word.end;
    }
    anchors
}

fn matched_anchors(text: &[char], reference: &[Anchor]) -> Vec<(usize, usize)> {
    let columns = reference.len() + 1;
    let mut scores = vec![0_i16; (text.len() + 1) * columns];
    for (i, character) in text.iter().enumerate() {
        let row = (i + 1) * columns;
        scores[row] = -2 * (i as i16 + 1);
        for (j, anchor) in reference.iter().enumerate() {
            let cell = row + j + 1;
            let skip_text = scores[cell - columns] - 2;
            let skip_audio = scores[cell - 1] - 1;
            let score = if character == &anchor.character {
                (scores[cell - columns - 1] + 3)
                    .max(skip_text)
                    .max(skip_audio)
            } else {
                skip_text.max(skip_audio)
            };
            scores[cell] = score;
        }
    }
    let mut i = text.len();
    let mut j = scores[i * columns..]
        .iter()
        .enumerate()
        .max_by_key(|(index, score)| (**score, std::cmp::Reverse(*index)))
        .map_or(0, |(index, _)| index);
    let mut matched = Vec::new();
    while i > 0 && j > 0 {
        let cell = i * columns + j;
        if text[i - 1] == reference[j - 1].character
            && scores[cell] == scores[cell - columns - 1] + 3
        {
            matched.push((i - 1, j - 1));
            i -= 1;
            j -= 1;
        } else if scores[cell] == scores[cell - columns] - 2 {
            i -= 1;
        } else {
            j -= 1;
        }
    }
    matched.reverse();
    matched
}

fn word_boundaries(
    endpoints: &[usize],
    amount: usize,
    matched: &[(usize, usize)],
    window: &[Anchor],
) -> Vec<f32> {
    let start = window[matched[0].1].start;
    let end = window[matched[matched.len() - 1].1].end;
    let mut points = Vec::with_capacity(matched.len() + 2);
    points.push((0, start));
    points.extend(matched.iter().filter_map(|&(index, anchor)| {
        (index + 1 < amount).then_some((index + 1, window[anchor].end))
    }));
    points.push((amount, end));
    endpoints
        .iter()
        .map(|&index| {
            let offset = points.partition_point(|&(position, _)| position < index);
            if offset == 0 {
                return start;
            }
            let (left, from) = points[offset - 1];
            let (right, to) = points[offset];
            let portion = (index - left) as f32 / (right - left) as f32;
            let anchored = from + (to - from) * portion;
            let linear = start + (end - start) * index as f32 / amount as f32;
            anchored * 0.98 + linear * 0.02
        })
        .collect()
}

fn align_to_reference(segments: &mut [CaptionSegment], reference: &[Anchor]) -> Result<(), String> {
    let mut cursor = 0;
    let mut supported = 0;
    let mut spoken = 0;
    for segment in segments.iter_mut() {
        let mut characters = Vec::new();
        let mut ends = Vec::new();
        for word in &segment.words {
            characters.extend(spoken_characters(&word.text));
            ends.push(characters.len());
        }
        if characters.is_empty() || characters.len() > 256 {
            return Err(tr::editor::regenerate::unverified().into());
        }
        let low = reference
            .partition_point(|anchor| anchor.end < segment.start - MAX_REFERENCE_SECONDS)
            .max(cursor);
        let high =
            reference.partition_point(|anchor| anchor.start <= segment.end + MAX_REFERENCE_SECONDS);
        let window = reference.get(low..high).unwrap_or_default();
        let matched = matched_anchors(&characters, window);
        let count = matched.len();
        if count < characters.len().div_ceil(2) {
            tracing::warn!(
                matched = count,
                characters = characters.len(),
                start = segment.start,
                "caption alignment rejected a cue"
            );
            return Err(tr::editor::regenerate::unverified().into());
        }
        let first = matched[0].1;
        let last = matched[count - 1].1;
        let start = window[first].start;
        let end = window[last].end;
        if !(end > start && end - start <= (segment.end - segment.start).max(1.0) + 5.0) {
            return Err(tr::editor::regenerate::unverified().into());
        }
        let mut previous = start;
        let boundaries = word_boundaries(&ends, characters.len(), &matched, window);
        for (word, boundary) in segment.words.iter_mut().zip(boundaries) {
            word.start = previous;
            word.end = boundary;
            previous = boundary;
        }
        segment.start = start;
        segment.end = end;
        cursor = low + last + 1;
        supported += count;
        spoken += characters.len();
    }
    if spoken == 0
        || supported as f32 / (spoken.max(reference.len()) as f32) < MIN_ALIGNMENT_COVERAGE
    {
        tracing::warn!(
            supported,
            spoken,
            reference = reference.len(),
            "caption alignment coverage was too low"
        );
        return Err(tr::editor::regenerate::unverified().into());
    }
    Ok(())
}

fn verify_ranges(segments: &[CaptionSegment], duration: f32) -> Result<(), String> {
    let mut previous_end = 0.0;
    if segments.is_empty() {
        return Err(tr::editor::regenerate::unverified().into());
    }
    for segment in segments {
        let span = segment.end - segment.start;
        let units = spoken_characters(&segment.text)
            .map(|c| if c.is_ascii() { 0.45 } else { 1.0 })
            .sum::<f32>();
        if !(segment.start >= previous_end - 0.03
            && span > 0.03
            && segment.end <= duration + 0.03
            && units <= span * 18.0 + 4.0)
        {
            return Err(tr::editor::regenerate::unverified().into());
        }
        previous_end = segment.end;
    }
    if crate::asr_cloud::has_repeated_passage(
        &segments
            .iter()
            .map(|segment| segment.text.as_str())
            .collect::<Vec<_>>()
            .join(" "),
    ) {
        return Err(tr::editor::regenerate::unverified().into());
    }
    Ok(())
}

pub(crate) fn generate(
    path: &Path,
    source: &CaptionRecognition,
    ranges: &[Range<f64>],
    durations: &[f64],
) -> Result<Vec<CaptionSegment>, String> {
    if recognition(&source.engine, &source.language) != *source {
        return Err(tr::editor::regenerate::model_changed().into());
    }
    let whisper = matches!(source.engine.as_str(), "small" | "medium");
    let _worker = lock_transcription_worker_slot();
    let result = (|| {
        let context = if whisper {
            if !model_files_present(&source.engine) || hub().downloading(&source.engine) {
                return Err(tr::transcription::error::model_missing().into());
            }
            Some(get_whisper_context(
                model_path(&source.engine).to_string_lossy().as_ref(),
            )?)
        } else {
            None
        };
        let mut result = Vec::new();
        for range in ranges {
            let segments = with_transcription_audio(
                path,
                std::slice::from_ref(range),
                durations,
                &AtomicBool::new(false),
                |wav, offsets| {
                    let samples = read_wav_16k_mono(wav)?;
                    let mut recognition = if let Some(context) = &context {
                        let groups = process_with_whisper(
                            &samples,
                            context.clone(),
                            &source.language,
                            &[],
                            &VerbatimOptions::default().first_pass(),
                        )?;
                        crate::asr_cloud::VerifiedRecognition {
                            segments: caption_segments_from_groups(groups, &source.language),
                            reference: None,
                        }
                    } else if source.engine == crate::local_qwen::MODEL {
                        crate::local_qwen::verified(&samples)?
                    } else {
                        crate::asr_cloud::verify_cloud(
                            &source.engine,
                            &source.model,
                            &samples,
                            &user_transcription_hints(),
                        )?
                    };
                    if let Some(reference) = recognition.reference.take() {
                        let reference: Vec<_> =
                            reference.into_iter().map(|segment| segment.words).collect();
                        align_to_reference(
                            &mut recognition.segments,
                            &reference_anchors(&reference),
                        )?;
                    }
                    let segments = recognition.segments;
                    verify_ranges(&segments, samples.len() as f32 / WHISPER_SAMPLE_RATE as f32)?;
                    Ok(restore_source_timestamps(segments, offsets))
                },
            )?;
            result.extend(segments);
        }
        if result.is_empty() {
            return Err(tr::editor::regenerate::unverified().into());
        }
        for segment in &mut result {
            segment.id = format!("regenerated-{}", uuid::Uuid::new_v4());
        }
        Ok(result)
    })();
    if whisper {
        release_whisper_context_after_transcription();
    }
    result
}

pub(crate) fn apply(
    project: &mut ProjectConfiguration,
    source: CaptionRecognition,
    mut segments: Vec<CaptionSegment>,
    ranges: &[Range<f64>],
    full: bool,
    durations: &[f64],
) {
    if full {
        if let Some(captions) = &mut project.captions {
            captions.settings.translations.clear();
            captions.settings.display_mode = cap_project::CaptionDisplayMode::Original;
        }
        apply_caption_result(
            project,
            segments,
            &source.language,
            durations,
            durations.iter().sum(),
        );
    } else if let Some(captions) = &mut project.captions {
        captions.segments.retain(|existing| {
            let keep = !ranges
                .iter()
                .any(|range| overlaps(f64::from(existing.start), f64::from(existing.end), range));
            if !keep {
                captions
                    .settings
                    .translations
                    .retain(|id, _| source_caption_id(id) != existing.id);
            }
            keep
        });
        captions.segments.append(&mut segments);
        captions
            .segments
            .sort_by(|a, b| a.start.total_cmp(&b.start));
        cap_project::synchronize_captions(project, durations);
    }
    project.caption_recognition = Some(source);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn segment(id: &str, start: f32, end: f32, text: &str) -> CaptionSegment {
        CaptionSegment {
            id: id.into(),
            start,
            end,
            text: text.into(),
            manual_timing: None,
            words: vec![CaptionWord {
                text: text.into(),
                start,
                end,
                hidden: false,
            }],
        }
    }

    #[test]
    fn detect_a_dense_run_without_removing_the_previous_line_at_its_boundary() {
        let clean = segment("good", 0.0, 2.000_01, "This line was checked");
        let mut project = ProjectConfiguration {
            captions: Some(cap_project::CaptionsData {
                source_timed: true,
                segments: vec![
                    clean.clone(),
                    segment("repeat-a", 2.0, 2.1, "重复的字幕"),
                    segment("repeat-b", 2.1, 2.2, "重复的字幕"),
                    segment("repeat-c", 2.2, 2.3, "重复的字幕"),
                    segment("last", 2.5, 3.8, "最后一行是好的"),
                ],
                ..Default::default()
            }),
            ..Default::default()
        };
        let visible = std::iter::once(0.0..4.0).collect::<Vec<_>>();
        let mut history = crate::editor_edits::ProjectHistory::new(project.clone());
        let ranges = suspicious_ranges(&project, &visible);
        assert_eq!(ranges.len(), 1);
        assert_eq!(ranges[0].start, 2.0);
        apply(
            &mut project,
            recognition("small", "zh"),
            vec![segment("new", 2.0, 2.3, "新的")],
            &ranges,
            false,
            &[4.0],
        );
        history.record(&project);
        let restored = history.undo().unwrap();
        assert!(restored.caption_recognition.is_none());
        assert_eq!(restored.captions.as_ref().unwrap().segments.len(), 5);
        assert!(!history.can_undo());
        let captions = project.captions.unwrap();
        assert_eq!(
            captions
                .segments
                .iter()
                .map(|segment| segment.id.as_str())
                .collect::<Vec<_>>(),
            ["good", "new", "last"]
        );
        assert_eq!(captions.segments[0].text, clean.text);
        assert_eq!(project.caption_recognition.unwrap().engine, "small");
    }

    #[test]
    fn audio_alignment_moves_the_cue_and_rejects_unrelated_text() {
        let spoken = segment("spoken", 2.0, 4.0, "可以播放这段原音");
        let reference = reference_anchors(&[spoken.words]);
        let mut candidate = [segment("draft", 0.0, 5.0, "可以播放这段原音")];
        align_to_reference(&mut candidate, &reference).unwrap();
        assert!((candidate[0].start - 2.0).abs() < 0.01);
        assert!((candidate[0].end - 4.0).abs() < 0.01);
        let mut invented = [segment("draft", 0.0, 5.0, "今天晴窗照海岸")];
        assert!(align_to_reference(&mut invented, &reference).is_err());
    }

    #[test]
    #[ignore]
    fn verified_regeneration_real_recording() {
        let _ = tracing_subscriber::fmt().with_test_writer().try_init();
        let path = std::env::var_os("CAPWISE_REGEN_TEST_PROJECT").unwrap();
        let output = std::env::var_os("CAPWISE_REGEN_TEST_OUTPUT").unwrap();
        let path = Path::new(&path);
        let project = ProjectConfiguration::load(path).unwrap();
        let duration = project.timeline.as_ref().unwrap().segments[0].end;
        let durations = [duration];
        let visible =
            cap_project::transcription_source_ranges(project.timeline.as_ref(), &durations);
        let spans = suspicious_ranges(&project, &visible);
        assert_eq!(spans.len(), 1, "{spans:?}");
        assert!(spans[0].start > 42.0 && spans[0].start < 49.0, "{spans:?}");
        let model = recognition("qwen3-asr", "zh");
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap();
        for (name, ranges, full) in [("partial", spans, false), ("full", visible, true)] {
            let (found, updated) = runtime.block_on(async {
                let task_path = path.to_path_buf();
                let task_model = model.clone();
                let task_project = project.clone();
                tokio::task::spawn_blocking(move || {
                    let found = generate(&task_path, &task_model, &ranges, &durations).unwrap();
                    let mut next = task_project;
                    apply(
                        &mut next,
                        task_model,
                        found.clone(),
                        &ranges,
                        full,
                        &durations,
                    );
                    (found, next)
                })
                .await
                .unwrap()
            });
            assert!(found.len() < 70, "{name}: {}", found.len());
            let tail = found
                .iter()
                .map(|caption| caption.text.as_str())
                .collect::<Vec<_>>()
                .join("");
            assert!(tail.contains("网页"), "{name}: missing end");
            let output = Path::new(&output).join(format!("{name}.json"));
            std::fs::write(output, serde_json::to_vec_pretty(&updated).unwrap()).unwrap();
        }
    }
}
