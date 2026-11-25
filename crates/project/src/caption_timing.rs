use crate::caption_words::{MAX_CAPTION_WORD_DURATION, caption_text_from_words};
use crate::{
    CaptionSegment, CaptionTrackSegment, CaptionWord, ProjectConfiguration, TimelineConfiguration,
};
use either::Either;
use std::collections::HashMap;

#[cfg(test)]
mod unordered_projection_tests;

pub(crate) fn clip_timeline_offsets(timeline: &TimelineConfiguration) -> Vec<f64> {
    let mut offset = 0.0;
    timeline
        .segments
        .iter()
        .enumerate()
        .map(|(index, segment)| {
            offset -= timeline
                .effective_transition(index)
                .map_or(0.0, |transition| transition.duration);
            let start = offset;
            offset += segment.duration();
            start
        })
        .collect()
}

const CAPTION_EDL_SEPARATOR: &str = "::edl";

pub fn source_caption_id(track_id: &str) -> &str {
    track_id
        .find(CAPTION_EDL_SEPARATOR)
        .map_or(track_id, |index| &track_id[..index])
}

fn mapped_caption_segment_id(base_id: &str, index: usize, total: usize) -> String {
    if total == 1 {
        base_id.to_string()
    } else {
        format!("{base_id}{CAPTION_EDL_SEPARATOR}{index}")
    }
}

fn clamp_caption_segment_words(segment: &CaptionSegment) -> CaptionSegment {
    if segment.words.is_empty() {
        return segment.clone();
    }

    let clamped_words: Vec<CaptionWord> = segment
        .words
        .iter()
        .map(|word| CaptionWord {
            text: word.text.clone(),
            start: word.start,
            end: word.end.min(word.start + MAX_CAPTION_WORD_DURATION),
            hidden: word.hidden,
        })
        .collect();

    let last_word_end = clamped_words.last().map_or(segment.end, |word| word.end);

    CaptionSegment {
        id: segment.id.clone(),
        start: segment.start,
        end: segment.end.min(last_word_end),
        text: segment.text.clone(),
        words: clamped_words,
        manual_timing: None,
    }
}

fn retime_caption_words(caption: &CaptionSegment, start: f32, end: f32) -> Vec<CaptionWord> {
    let retime = manual_caption_word_timing(caption, [start, end]);
    caption
        .words
        .iter()
        .cloned()
        .map(|mut word| {
            (word.start, word.end) = retime(&word);
            word
        })
        .collect()
}

fn manual_caption_word_timing(
    caption: &CaptionSegment,
    [start, end]: [f32; 2],
) -> impl Fn(&CaptionWord) -> (f32, f32) + use<> {
    let first_start = caption
        .words
        .first()
        .map_or(caption.start, |word| word.start);
    let last_end = caption.words.last().map_or(caption.end, |word| word.end);
    let old_duration = last_end - first_start;
    let time = move |value: f32| {
        let progress = ((value - first_start) / old_duration).clamp(0.0, 1.0);
        start + progress * (end - start)
    };
    move |word| {
        if old_duration.is_finite()
            && old_duration > 0.0
            && word.start.is_finite()
            && word.end.is_finite()
        {
            (time(word.start), time(word.end))
        } else {
            (start, end)
        }
    }
}

struct SourceToEditedMapping {
    source_start: f64,
    source_end: f64,
    edited_start: f64,
    timescale: f64,
}

impl SourceToEditedMapping {
    fn edited_end(&self) -> f64 {
        self.edited_start + (self.source_end - self.source_start) / self.timescale
    }
}

fn source_time_at(mappings: &[SourceToEditedMapping], effective: f64) -> Option<f32> {
    let end = mappings.partition_point(|mapping| mapping.edited_start <= effective);
    let mapping = mappings
        .get(end.checked_sub(1)?)
        .filter(|mapping| effective <= mapping.edited_end())?;
    Some((mapping.source_start + (effective - mapping.edited_start) * mapping.timescale) as f32)
}

fn build_source_to_edited_mappings(
    timeline: &TimelineConfiguration,
    recording_durations: &[f64],
) -> Vec<SourceToEditedMapping> {
    let mut recording_offsets = Vec::with_capacity(recording_durations.len());
    let mut cumulative = 0.0;
    for duration in recording_durations {
        recording_offsets.push(cumulative);
        cumulative += duration;
    }

    let edited_offsets = clip_timeline_offsets(timeline);

    timeline
        .segments
        .iter()
        .zip(edited_offsets)
        .map(|(segment, edited_start)| {
            let recording_offset = recording_offsets
                .get(segment.recording_clip as usize)
                .copied()
                .unwrap_or(0.0);
            SourceToEditedMapping {
                source_start: recording_offset + segment.start,
                source_end: recording_offset + segment.end,
                edited_start,
                timescale: segment.timescale,
            }
        })
        .collect()
}

pub fn transcription_source_ranges(
    timeline: Option<&TimelineConfiguration>,
    recording_durations: &[f64],
) -> Vec<std::ops::Range<f64>> {
    let Some(timeline) = timeline else {
        let duration = recording_durations.iter().sum();
        return if duration > 0.0 {
            vec![0.0..duration]
        } else {
            Vec::new()
        };
    };
    let mut ranges: Vec<_> = build_source_to_edited_mappings(timeline, recording_durations)
        .into_iter()
        .zip(&timeline.segments)
        .filter_map(|(mapping, segment)| {
            let duration = *recording_durations.get(segment.recording_clip as usize)?;
            let offset = mapping.source_start - segment.start;
            let start = segment.start.max(0.0);
            let end = segment.end.min(duration);
            (start < end).then_some(offset + start..offset + end)
        })
        .collect();
    ranges.sort_by(|a, b| a.start.total_cmp(&b.start));
    let mut merged: Vec<std::ops::Range<f64>> = Vec::with_capacity(ranges.len());
    for range in ranges {
        match merged.last_mut() {
            Some(last) if range.start <= last.end => last.end = last.end.max(range.end),
            _ => merged.push(range),
        }
    }
    merged
}

pub fn transcription_ranges_cover(
    transcribed: &[std::ops::Range<f64>],
    required: &[std::ops::Range<f64>],
) -> bool {
    required.iter().all(|range| {
        transcribed
            .iter()
            .any(|source| source.start <= range.start && source.end >= range.end)
    })
}

fn map_time_range_within_mapping(
    start: f64,
    end: f64,
    mapping: &SourceToEditedMapping,
) -> Option<(f64, f64)> {
    let overlap_start = start.max(mapping.source_start);
    let overlap_end = end.min(mapping.source_end);
    if overlap_start >= overlap_end {
        return None;
    }
    Some((
        mapping.edited_start + (overlap_start - mapping.source_start) / mapping.timescale,
        mapping.edited_start + (overlap_end - mapping.source_start) / mapping.timescale,
    ))
}

struct IndexedSourceMapping {
    mapping_index: usize,
    maximum_end: f64,
}

fn index_source_mappings(mappings: &[SourceToEditedMapping]) -> Option<Vec<IndexedSourceMapping>> {
    if !mappings.iter().all(|mapping| {
        mapping.source_start.is_finite()
            && mapping.source_end.is_finite()
            && mapping.source_end >= mapping.source_start
    }) {
        return None;
    }
    let mut index: Vec<_> = mappings
        .iter()
        .enumerate()
        .map(|(mapping_index, mapping)| IndexedSourceMapping {
            mapping_index,
            maximum_end: mapping.source_end,
        })
        .collect();
    index.sort_unstable_by(|left, right| {
        mappings[left.mapping_index]
            .source_start
            .total_cmp(&mappings[right.mapping_index].source_start)
    });
    let mut maximum_end = f64::NEG_INFINITY;
    for entry in &mut index {
        maximum_end = maximum_end.max(entry.maximum_end);
        entry.maximum_end = maximum_end;
    }
    Some(index)
}

fn effective_to_output(holds: &[(f64, f64)], effective: f64) -> f64 {
    let mut output = effective;
    for (start, end) in holds {
        if output >= *start {
            output += end - start;
        } else {
            break;
        }
    }
    output
}

fn effective_to_output_end(holds: &[(f64, f64)], effective: f64) -> f64 {
    let mut output = effective;
    for (start, end) in holds {
        if output > *start {
            output += end - start;
        } else {
            break;
        }
    }
    output
}

struct MappedCaption {
    id: String,
    start: f64,
    end: f64,
    text: String,
    words: Vec<CaptionWord>,
    manual_timing: bool,
}

fn map_captions_to_edited_timeline(
    raw_segments: &[CaptionSegment],
    timeline: &TimelineConfiguration,
    recording_durations: &[f64],
) -> Vec<MappedCaption> {
    if timeline.segments.is_empty() || recording_durations.is_empty() {
        return raw_segments
            .iter()
            .map(|caption| {
                if let Some([start, end]) = caption.manual_timing.filter(valid_manual_timing) {
                    return MappedCaption {
                        id: caption.id.clone(),
                        start: start.into(),
                        end: end.into(),
                        text: caption.text.clone(),
                        words: retime_caption_words(caption, start, end),
                        manual_timing: true,
                    };
                }
                let segment = clamp_caption_segment_words(caption);
                MappedCaption {
                    id: segment.id,
                    start: f64::from(segment.start),
                    end: f64::from(segment.end),
                    text: segment.text,
                    words: segment.words,
                    manual_timing: false,
                }
            })
            .collect();
    }

    let mappings = build_source_to_edited_mappings(timeline, recording_durations);
    let mappings_ordered = mappings
        .iter()
        .all(|mapping| mapping.source_start.is_finite() && mapping.source_end.is_finite())
        && mappings.windows(2).all(|pair| {
            pair[0].source_start <= pair[1].source_start && pair[0].source_end <= pair[1].source_end
        });
    let source_index = if !mappings_ordered && mappings.len() >= 32 && raw_segments.len() >= 16 {
        index_source_mappings(&mappings)
    } else {
        None
    };
    let mut candidates = Vec::new();
    let holds = timeline.hold_windows();
    let hold_adjusted = |start: f64, end: f64| {
        if holds.is_empty() {
            (start, end)
        } else {
            (
                effective_to_output(&holds, start),
                effective_to_output_end(&holds, end),
            )
        }
    };

    let mut result = Vec::new();

    for caption in raw_segments {
        let mut mapped_caption_segments: Vec<MappedCaption> = Vec::new();
        let manual_range = caption.manual_timing.filter(valid_manual_timing);
        let retime = manual_range.map(|range| manual_caption_word_timing(caption, range));
        let words = &caption.words;
        let word_range = |word: &CaptionWord| {
            retime.as_ref().map_or_else(
                || {
                    (
                        word.start,
                        word.end.min(word.start + MAX_CAPTION_WORD_DURATION),
                    )
                },
                |retime| retime(word),
            )
        };
        let word_bounds = if words.is_empty() || manual_range.is_some() {
            None
        } else {
            words
                .iter()
                .try_fold((f64::INFINITY, f64::NEG_INFINITY), |(start, end), word| {
                    let (word_start, word_end) = word_range(word);
                    let word_start = f64::from(word_start);
                    let word_end = f64::from(word_end);
                    if word_start.is_nan() || word_end.is_nan() {
                        None
                    } else {
                        Some((start.min(word_start), end.max(word_end)))
                    }
                })
        };

        let bounds = if let Some([start, end]) = manual_range {
            Some((f64::from(start), f64::from(end)))
        } else if words.is_empty() {
            Some((f64::from(caption.start), f64::from(caption.end)))
        } else {
            word_bounds
        };
        let caption_mappings = match (bounds, source_index.as_deref()) {
            (Some((start, end)), _) if mappings_ordered && start.is_finite() && end.is_finite() => {
                let first = mappings.partition_point(|mapping| mapping.source_end <= start);
                let count = mappings[first..].partition_point(|mapping| mapping.source_start < end);
                Either::Left(mappings[first..first + count].iter())
            }
            (Some((start, end)), Some(index))
                if start.is_finite() && end.is_finite() && start < end =>
            {
                let first = index.partition_point(|entry| entry.maximum_end <= start);
                let count = index[first..]
                    .partition_point(|entry| mappings[entry.mapping_index].source_start < end);
                if count >= mappings.len() / 2 {
                    Either::Left(mappings.iter())
                } else {
                    candidates.clear();
                    candidates.extend(index[first..first + count].iter().filter_map(|entry| {
                        (mappings[entry.mapping_index].source_end > start)
                            .then_some(entry.mapping_index)
                    }));
                    candidates.sort_unstable();
                    Either::Right(candidates.iter().map(|index| &mappings[*index]))
                }
            }
            _ => Either::Left(mappings.iter()),
        };

        for mapping in caption_mappings {
            let manual_bounds = manual_range.and_then(|[start, end]| {
                map_time_range_within_mapping(start.into(), end.into(), mapping)
                    .map(|(start, end)| hold_adjusted(start, end))
            });
            if manual_range.is_some() && manual_bounds.is_none() {
                continue;
            }
            if let Some((start, end)) = word_bounds
                && (end <= mapping.source_start || start >= mapping.source_end)
            {
                continue;
            }
            let mut mapped_words = Vec::new();
            for word in words {
                let (word_start, word_end) = word_range(word);
                let Some((start, end)) =
                    map_time_range_within_mapping(word_start.into(), word_end.into(), mapping)
                else {
                    continue;
                };
                let (start, end) = hold_adjusted(start, end);
                mapped_words.push(CaptionWord {
                    text: word.text.clone(),
                    start: start as f32,
                    end: end as f32,
                    hidden: word.hidden,
                });
            }
            let (start, end, text) = if let Some((start, end)) = manual_bounds {
                if mapped_words.is_empty() {
                    mapped_words.extend(words.iter().cloned().map(|mut word| {
                        // The cue spans this clip, but none of its word highlights do.
                        (word.start, word.end) = (start as f32, start as f32);
                        word
                    }));
                }
                let text = if words.is_empty() {
                    caption.text.clone()
                } else {
                    caption_text_from_words(&mapped_words)
                };
                (start, end, text)
            } else if words.is_empty() {
                let Some((start, end)) = map_time_range_within_mapping(
                    f64::from(caption.start),
                    f64::from(caption.end),
                    mapping,
                ) else {
                    continue;
                };
                let (start, end) = hold_adjusted(start, end);
                (start, end, caption.text.clone())
            } else {
                let (Some(first), Some(last)) = (mapped_words.first(), mapped_words.last()) else {
                    continue;
                };
                (
                    f64::from(first.start),
                    f64::from(last.end),
                    caption_text_from_words(&mapped_words),
                )
            };
            mapped_caption_segments.push(MappedCaption {
                id: caption.id.clone(),
                start,
                end,
                text,
                words: mapped_words,
                manual_timing: manual_range.is_some(),
            });
        }

        let total = mapped_caption_segments.len();
        for (index, mut segment) in mapped_caption_segments.into_iter().enumerate() {
            segment.id = mapped_caption_segment_id(&caption.id, index, total);
            result.push(segment);
        }
    }

    result
}

fn valid_manual_timing([start, end]: &[f32; 2]) -> bool {
    start.is_finite() && end.is_finite() && start < end
}

pub fn derive_caption_track_segments(
    source_segments: &[CaptionSegment],
    timeline: &TimelineConfiguration,
    recording_durations: &[f64],
) -> Vec<CaptionTrackSegment> {
    let mut overrides_by_source_id = HashMap::new();
    for segment in &timeline.caption_segments {
        overrides_by_source_id
            .entry(source_caption_id(&segment.id))
            .or_insert(segment);
    }

    let mut mapped =
        map_captions_to_edited_timeline(source_segments, timeline, recording_durations);
    mapped.sort_by(|a, b| a.start.total_cmp(&b.start));

    mapped
        .into_iter()
        .map(|segment| {
            let overrides = overrides_by_source_id
                .get(source_caption_id(&segment.id))
                .copied();
            mapped_track_segment(segment, overrides)
        })
        .collect()
}

fn mapped_track_segment(
    segment: MappedCaption,
    overrides: Option<&CaptionTrackSegment>,
) -> CaptionTrackSegment {
    CaptionTrackSegment {
        id: segment.id,
        start: segment.start,
        end: segment.end,
        text: segment.text,
        words: segment.words,
        manual_timing: segment.manual_timing,
        fade_duration_override: overrides.and_then(|o| o.fade_duration_override),
        linger_duration_override: overrides.and_then(|o| o.linger_duration_override),
        position_override: overrides.and_then(|o| o.position_override.clone()),
        color_override: overrides.and_then(|o| o.color_override.clone()),
        background_color_override: overrides.and_then(|o| o.background_color_override.clone()),
        font_size_override: overrides.and_then(|o| o.font_size_override),
    }
}

pub fn update_caption_text_projection(
    project: &mut ProjectConfiguration,
    source_id: &str,
    recording_durations: &[f64],
) -> Option<Vec<usize>> {
    update_caption_projection(project, source_id, recording_durations, true)
}

pub fn update_caption_timing_projection(
    project: &mut ProjectConfiguration,
    source_id: &str,
    recording_durations: &[f64],
) -> Option<Vec<usize>> {
    update_caption_projection(project, source_id, recording_durations, false)
}

fn update_caption_projection(
    project: &mut ProjectConfiguration,
    source_id: &str,
    recording_durations: &[f64],
    keep_timing: bool,
) -> Option<Vec<usize>> {
    let captions = project
        .captions
        .as_ref()
        .filter(|captions| captions.source_timed)?;
    let mut sources = captions
        .segments
        .iter()
        .filter(|segment| segment.id == source_id);
    let source = sources.next()?;
    if sources.next().is_some() {
        return None;
    }
    let timeline = project.timeline.as_mut()?;
    if timeline.segments.is_empty() || recording_durations.is_empty() {
        return None;
    }
    let mut mapped = map_captions_to_edited_timeline(
        std::slice::from_ref(source),
        timeline,
        recording_durations,
    );
    mapped.sort_by(|a, b| a.start.total_cmp(&b.start));
    let indices: Vec<_> = timeline
        .caption_segments
        .iter()
        .enumerate()
        .filter_map(|(index, segment)| {
            (source_caption_id(&segment.id) == source_id).then_some(index)
        })
        .collect();
    if mapped.len() != indices.len()
        || mapped.iter().zip(&indices).any(|(next, &index)| {
            let current = &timeline.caption_segments[index];
            if next.id != current.id {
                return true;
            }
            if keep_timing {
                return next.start.to_bits() != current.start.to_bits()
                    || next.end.to_bits() != current.end.to_bits();
            }
            if next.start.to_bits() == current.start.to_bits() {
                return false;
            }
            let previous = index
                .checked_sub(1)
                .and_then(|i| timeline.caption_segments.get(i));
            let following = timeline.caption_segments.get(index + 1);
            previous.is_some_and(|segment| {
                source_caption_id(&segment.id) != source_id
                    && next.start.total_cmp(&segment.start).is_le()
            }) || following.is_some_and(|segment| {
                source_caption_id(&segment.id) != source_id
                    && next.start.total_cmp(&segment.start).is_ge()
            })
        })
    {
        return None;
    }
    let overrides = indices
        .first()
        .map(|&index| &timeline.caption_segments[index]);
    let replacements: Vec<_> = mapped
        .into_iter()
        .map(|segment| mapped_track_segment(segment, overrides))
        .collect();
    for (&index, segment) in indices.iter().zip(replacements) {
        timeline.caption_segments[index] = segment;
    }
    Some(indices)
}

pub fn synchronize_captions(project: &mut ProjectConfiguration, recording_durations: &[f64]) {
    let (Some(captions), Some(timeline)) = (&mut project.captions, &mut project.timeline) else {
        return;
    };
    if recording_durations.is_empty() || timeline.segments.is_empty() {
        return;
    }
    if !captions.source_timed {
        let holds = timeline.hold_windows();
        let mappings = build_source_to_edited_mappings(timeline, recording_durations);
        let mappings_ordered = mappings.len() >= 128
            && mappings.iter().all(|mapping| {
                mapping.edited_start.is_finite()
                    && mapping.edited_end().is_finite()
                    && mapping.edited_end() >= mapping.edited_start
            })
            && mappings.windows(2).all(|pair| {
                pair[0].edited_start <= pair[1].edited_start
                    && pair[0].edited_end() <= pair[1].edited_end()
            });
        let output_end = timeline.duration() as f32;
        let to_source = |output: f32| -> Option<f32> {
            if output == output_end {
                return mappings.last().map(|mapping| mapping.source_end as f32);
            }
            let output = f64::from(output);
            let effective = output
                - holds
                    .iter()
                    .map(|(start, end)| (output.min(*end) - start).max(0.0))
                    .sum::<f64>();
            if mappings_ordered {
                return source_time_at(&mappings, effective);
            }
            mappings.iter().rev().find_map(|mapping| {
                let end = mapping.edited_start
                    + (mapping.source_end - mapping.source_start) / mapping.timescale;
                (effective >= mapping.edited_start && effective <= end).then_some({
                    (mapping.source_start + (effective - mapping.edited_start) * mapping.timescale)
                        as f32
                })
            })
        };
        captions.segments = captions
            .segments
            .iter()
            .filter_map(|caption| {
                let start = to_source(caption.start)?;
                let end = to_source(caption.end)?;
                let manual_timing = caption.manual_timing.and_then(|[start, end]| {
                    let range = [to_source(start)?, to_source(end)?];
                    valid_manual_timing(&range).then_some(range)
                });
                let words = caption
                    .words
                    .iter()
                    .filter_map(|word| {
                        Some(CaptionWord {
                            text: word.text.clone(),
                            start: to_source(word.start)?,
                            end: to_source(word.end)?,
                            hidden: word.hidden,
                        })
                    })
                    .collect();
                Some(CaptionSegment {
                    id: caption.id.clone(),
                    start,
                    end,
                    text: caption.text.clone(),
                    words,
                    manual_timing,
                })
            })
            .collect();
        captions
            .segments
            .sort_by(|a, b| a.start.total_cmp(&b.start));
        captions.source_timed = true;
    }
    timeline.caption_segments =
        derive_caption_track_segments(&captions.segments, timeline, recording_durations);
}

#[cfg(test)]
mod projection_tests;

#[cfg(test)]
mod migration_lookup_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CaptionsData;

    #[test]
    fn transcription_ranges_follow_source_cuts_independent_of_speed_and_clip_order() {
        let timeline = serde_json::from_value(serde_json::json!({
            "segments": [
                {"recordingSegment":1,"start":1.0,"end":3.0,"timescale":0.5},
                {"recordingSegment":0,"start":5.0,"end":6.0,"timescale":2.0},
                {"recordingSegment":0,"start":1.0,"end":3.0,"timescale":1.0},
                {"recordingSegment":0,"start":2.0,"end":4.0,"timescale":1.0},
                {"recordingSegment":0,"start":4.0,"end":4.5,"timescale":1.0},
                {"recordingSegment":0,"start":1.0,"end":3.0,"timescale":2.0}
            ], "zoomSegments": []
        }))
        .unwrap();
        assert_eq!(
            transcription_source_ranges(Some(&timeline), &[10.0, 4.0]),
            vec![1.0..4.5, 5.0..6.0, 11.0..13.0]
        );
    }

    #[test]
    fn transcription_ranges_preserve_empty_timelines_and_clip_to_sources() {
        let mut timeline: TimelineConfiguration =
            serde_json::from_value(serde_json::json!({"segments": [], "zoomSegments": []}))
                .unwrap();
        assert!(transcription_source_ranges(Some(&timeline), &[30.0]).is_empty());
        assert_eq!(
            transcription_source_ranges(None, &[30.0, 10.0]),
            vec![0.0..40.0]
        );
        timeline.segments = serde_json::from_value(serde_json::json!([
            {"recordingSegment":0,"start":-1.0,"end":4.0,"timescale":1.0},
            {"recordingSegment":0,"start":8.0,"end":40.0,"timescale":1.0},
            {"recordingSegment":2,"start":0.0,"end":3.0,"timescale":1.0},
            {"recordingSegment":0,"start":5.0,"end":5.0,"timescale":1.0}
        ]))
        .unwrap();
        assert_eq!(
            transcription_source_ranges(Some(&timeline), &[10.0]),
            vec![0.0..4.0, 8.0..10.0]
        );
    }

    #[test]
    fn transcription_coverage_accepts_identical_ranges() {
        let ranges = [1.0..4.0, 6.0..10.0];
        assert!(transcription_ranges_cover(&ranges, &ranges));
    }

    #[test]
    fn transcription_coverage_accepts_further_trims_and_reordered_clips() {
        let timeline = serde_json::from_value(serde_json::json!({
            "segments": [
                {"start":7.0,"end":9.0,"timescale":2.0},
                {"start":2.0,"end":3.0,"timescale":0.5}
            ], "zoomSegments": []
        }))
        .unwrap();
        let current = transcription_source_ranges(Some(&timeline), &[10.0]);
        assert!(transcription_ranges_cover(&[1.0..4.0, 6.0..10.0], &current));
    }

    #[test]
    fn transcription_coverage_rejects_audio_restored_by_undo() {
        let timeline = serde_json::from_value(serde_json::json!({
            "segments": [{"start":0.0,"end":10.0,"timescale":1.0}],
            "zoomSegments": []
        }))
        .unwrap();
        let current = transcription_source_ranges(Some(&timeline), &[10.0]);
        assert!(!transcription_ranges_cover(
            &[0.0..4.0, 6.0..10.0],
            &current
        ));
        assert!(!transcription_ranges_cover(&[1.0..10.0], &current));
        assert!(!transcription_ranges_cover(&[0.0..9.0], &current));
        assert!(!transcription_ranges_cover(&[], &current));
        assert!(!transcription_ranges_cover(
            &[0.0..4.0],
            &[0.0..4.0, 6.0..10.0]
        ));
    }

    #[test]
    fn transcription_coverage_accepts_an_empty_timeline() {
        assert!(transcription_ranges_cover(&[0.0..10.0], &[]));
        assert!(transcription_ranges_cover(&[], &[]));
    }

    fn project(source_timed: bool, start: f32, end: f32) -> ProjectConfiguration {
        ProjectConfiguration {
            timeline: Some(serde_json::from_value(serde_json::json!({
                "segments":[{"start":0.0,"end":5.0,"timescale":1.0},{"start":6.0,"end":10.0,"timescale":1.0}],
                "zoomSegments":[]
            })).unwrap()),
            captions: Some(CaptionsData { source_timed, segments:vec![CaptionSegment { manual_timing: None, id:"spoken".into(),start,end,text:"hello".into(),words:vec![CaptionWord { text:"hello".into(),start,end, hidden: false }] }], ..Default::default() }),
            ..Default::default()
        }
    }

    #[test]
    fn text_projection_updates_every_repeated_fragment_and_keeps_other_rows() {
        let mut project = project(true, 1.0, 2.0);
        let timeline = project.timeline.as_mut().unwrap();
        timeline.segments.push(timeline.segments[0].clone());
        timeline.segments[2].timescale = 2.0;
        let captions = project.captions.as_mut().unwrap();
        let mut unrelated = captions.segments[0].clone();
        unrelated.id = "unrelated".into();
        unrelated.start = 7.0;
        unrelated.end = 8.0;
        unrelated.words[0].start = 7.0;
        unrelated.words[0].end = 8.0;
        captions.segments.push(unrelated);
        synchronize_captions(&mut project, &[10.0]);
        let track = &mut project.timeline.as_mut().unwrap().caption_segments;
        track[0].font_size_override = Some(30);
        let original_unrelated = track[1].text.as_ptr();
        let segment = &mut project.captions.as_mut().unwrap().segments[0];
        segment.text = "中文".into();
        segment.words[0].text = "中文".into();

        let mut expected = project.clone();
        synchronize_captions(&mut expected, &[10.0]);
        assert_eq!(
            update_caption_text_projection(&mut project, "spoken", &[10.0]),
            Some(vec![0, 2])
        );
        let track = &project.timeline.as_ref().unwrap().caption_segments;
        let expected = &expected.timeline.as_ref().unwrap().caption_segments;
        assert_eq!(
            serde_json::to_value(track).unwrap(),
            serde_json::to_value(expected).unwrap()
        );
        assert_eq!(track[1].text.as_ptr(), original_unrelated);
        assert_eq!(track[2].font_size_override, Some(30));
    }

    #[test]
    fn text_projection_falls_back_without_writing_when_timing_changes() {
        let mut project = project(true, 1.0, 2.0);
        synchronize_captions(&mut project, &[10.0]);
        let before = serde_json::to_value(&project.timeline).unwrap();
        project.captions.as_mut().unwrap().segments[0].words[0].start = 1.5;
        assert_eq!(
            update_caption_text_projection(&mut project, "spoken", &[10.0]),
            None
        );
        assert_eq!(serde_json::to_value(&project.timeline).unwrap(), before);
    }

    #[test]
    fn source_captions_follow_a_cut_on_load_and_export() {
        let mut project = project(true, 8.0, 9.0);
        synchronize_captions(&mut project, &[10.0]);
        let track = &project.timeline.as_ref().unwrap().caption_segments;
        assert_eq!(track[0].start, 7.0);
        assert_eq!(track[0].end, 8.0);
        assert_eq!(track[0].words[0].start, 7.0);
        let saved = serde_json::to_string(&project).unwrap();
        let mut reopened: ProjectConfiguration = serde_json::from_str(&saved).unwrap();
        synchronize_captions(&mut reopened, &[10.0]);
        assert_eq!(serde_json::to_string(&reopened).unwrap(), saved);
    }

    #[test]
    fn legacy_caption_migration_converts_a_manual_range_to_source_time() {
        let mut project = project(false, 0.5, 1.0);
        let clips = &mut project.timeline.as_mut().unwrap().segments;
        clips.truncate(1);
        clips[0].start = 4.0;
        clips[0].end = 12.0;
        clips[0].timescale = 2.0;
        project.captions.as_mut().unwrap().segments[0].manual_timing = Some([0.25, 3.5]);
        synchronize_captions(&mut project, &[12.0]);
        let source = &project.captions.as_ref().unwrap().segments[0];
        assert_eq!(source.manual_timing, Some([4.5, 11.0]));
        let track = &project.timeline.as_ref().unwrap().caption_segments[0];
        assert_eq!((track.start, track.end), (0.25, 3.5));
        assert!(track.manual_timing);
    }

    #[test]
    fn timing_projection_updates_one_caption_and_falls_back_on_reorder() {
        let mut project = project(true, 0.5, 1.5);
        let captions = project.captions.as_mut().unwrap();
        let mut later = captions.segments[0].clone();
        later.id = "later".into();
        later.manual_timing = Some([2.0, 3.0]);
        captions.segments.push(later);
        let durations = [10.0];
        synchronize_captions(&mut project, &durations);

        project.captions.as_mut().unwrap().segments[0].manual_timing = Some([0.75, 2.0]);
        assert_eq!(
            update_caption_timing_projection(&mut project, "spoken", &durations),
            Some(vec![0])
        );
        let track = &project.timeline.as_ref().unwrap().caption_segments;
        assert_eq!((track[0].start, track[0].end), (0.75, 2.0));
        assert_eq!((track[1].start, track[1].end), (2.0, 3.0));

        project.captions.as_mut().unwrap().segments[0].manual_timing = Some([3.0, 5.0]);
        assert_eq!(
            update_caption_timing_projection(&mut project, "spoken", &durations),
            None
        );
        synchronize_captions(&mut project, &durations);
        let track = &project.timeline.as_ref().unwrap().caption_segments;
        assert_eq!(track[0].id, "later");
        assert_eq!((track[1].start, track[1].end), (3.0, 5.0));
    }

    #[test]
    fn timing_projection_defers_new_ties_to_source_order() {
        let mut project = project(true, 2.0, 3.0);
        let mut earlier = project.captions.as_ref().unwrap().segments[0].clone();
        earlier.id = "earlier".into();
        earlier.manual_timing = Some([1.0, 2.0]);
        project.captions.as_mut().unwrap().segments.push(earlier);
        let durations = [10.0];
        synchronize_captions(&mut project, &durations);
        project.captions.as_mut().unwrap().segments[0].manual_timing = Some([1.0, 3.0]);

        assert_eq!(
            update_caption_timing_projection(&mut project, "spoken", &durations),
            None
        );
        synchronize_captions(&mut project, &durations);
        assert_eq!(
            project.timeline.as_ref().unwrap().caption_segments[0].id,
            "spoken"
        );
    }

    #[test]
    fn a_cut_cannot_restore_hidden_words_in_a_manual_caption() {
        let mut project = project(true, 0.0, 1.0);
        let timeline = project.timeline.as_mut().unwrap();
        timeline.segments[0].end = 1.0;
        timeline.segments[1].start = 4.0;
        timeline.segments[1].end = 6.0;
        let captions = project.captions.as_mut().unwrap();
        let caption = &mut captions.segments[0];
        caption.manual_timing = Some([0.0, 10.0]);
        caption.words[0].hidden = true;
        caption.words.push(CaptionWord {
            text: "world".into(),
            start: 9.0,
            end: 10.0,
            hidden: true,
        });
        caption.text = "hello world".into();
        synchronize_captions(&mut project, &[10.0]);
        let track = &project.timeline.as_ref().unwrap().caption_segments;
        let text = crate::export_subtitles(
            track,
            &project.captions.as_ref().unwrap().settings,
            crate::SubtitleFormat::Srt,
        );
        assert!(text.is_empty(), "{text}");

        project.captions.as_mut().unwrap().segments[0].words[0].hidden = false;
        synchronize_captions(&mut project, &[10.0]);
        let text = crate::export_subtitles(
            &project.timeline.as_ref().unwrap().caption_segments,
            &project.captions.as_ref().unwrap().settings,
            crate::SubtitleFormat::Srt,
        );
        assert!(
            text.contains("00:00:01,000 --> 00:00:03,000\nhello\n"),
            "{text}"
        );
        assert!(!text.contains("world"), "{text}");
    }

    #[test]
    fn legacy_caption_migration_accounts_for_fullscreen_holds() {
        let mut project = project(false, 9.0, 10.0);
        project.timeline.as_mut().unwrap().text_segments.push(
            serde_json::from_value(
                serde_json::json!({"start":2.0,"end":4.0,"layout":"fullscreen"}),
            )
            .unwrap(),
        );
        synchronize_captions(&mut project, &[10.0]);
        assert!(project.captions.as_ref().unwrap().source_timed);
        assert_eq!(project.captions.as_ref().unwrap().segments[0].start, 8.0);
        let track = &project.timeline.as_ref().unwrap().caption_segments;
        assert_eq!(track[0].start, 9.0);
        assert_eq!(track[0].end, 10.0);
    }

    #[test]
    fn legacy_caption_migration_preserves_rounded_timeline_end() {
        let durations = [41.0 / 15.0, 82.0 / 15.0];
        for (timescale, hold_duration) in [
            (1.0, 0.0),
            (2.0, 0.0),
            (0.5, 0.0),
            (1.0, 0.1),
            (1.0, 10000.1),
        ] {
            let mut timeline: TimelineConfiguration = serde_json::from_value(serde_json::json!({
                "segments": [
                    {"recordingSegment": 0, "start": 0.0, "end": durations[0], "timescale": timescale},
                    {"recordingSegment": 1, "start": 0.0, "end": durations[1], "timescale": timescale}
                ],
                "transitions": [{"segmentIndex": 1, "type": "cross-fade", "duration": 0.2}],
                "zoomSegments": []
            }))
            .unwrap();
            if hold_duration > 0.0 {
                timeline.text_segments.push(
                    serde_json::from_value(serde_json::json!({
                        "start": 1.0, "end": 1.0 + hold_duration, "layout": "fullscreen"
                    }))
                    .unwrap(),
                );
            }
            let end = timeline.duration() as f32;
            let mut project = project(false, end - 0.5, end);
            project.timeline = Some(timeline);

            synchronize_captions(&mut project, &durations);

            let captions = project.captions.as_ref().unwrap();
            assert_eq!(captions.segments.len(), 1, "{timescale}, {hold_duration}");
            let source_end = durations.iter().sum::<f64>() as f32;
            assert_eq!(captions.segments[0].end, source_end);
            assert_eq!(captions.segments[0].words[0].end, source_end);
            let track = &project.timeline.as_ref().unwrap().caption_segments;
            assert_eq!(track.len(), 1);
            assert!((track[0].end - f64::from(end)).abs() <= f64::from(end * f32::EPSILON));
            let saved = serde_json::to_string(&project).unwrap();
            synchronize_captions(&mut project, &durations);
            assert_eq!(serde_json::to_string(&project).unwrap(), saved);
        }
    }

    #[test]
    fn legacy_caption_migration_rejects_times_beyond_rounded_timeline_end() {
        let mut project = project(false, 8.0, 9.0_f32.next_up());
        synchronize_captions(&mut project, &[10.0]);
        assert!(project.captions.as_ref().unwrap().segments.is_empty());
        assert!(
            project
                .timeline
                .as_ref()
                .unwrap()
                .caption_segments
                .is_empty()
        );
    }

    #[test]
    fn stale_caption_tracks_are_replaced_and_styles_preserved() {
        let mut project = project(true, 8.0, 9.0);
        project.timeline.as_mut().unwrap().caption_segments.push(serde_json::from_value(serde_json::json!({"id":"spoken::edl0","start":100.0,"end":101.0,"text":"hello","colorOverride":"#123456"})).unwrap());
        synchronize_captions(&mut project, &[10.0]);
        let track = &project.timeline.as_ref().unwrap().caption_segments;
        assert_eq!(track.len(), 1);
        assert_eq!(track[0].start, 7.0);
        assert_eq!(track[0].color_override.as_deref(), Some("#123456"));
        project.captions.as_mut().unwrap().segments.clear();
        synchronize_captions(&mut project, &[10.0]);
        assert!(
            project
                .timeline
                .as_ref()
                .unwrap()
                .caption_segments
                .is_empty()
        );
    }

    #[test]
    fn source_captions_keep_display_offsets_across_silent_takes() {
        let mut project = project(true, 6.0, 7.0);
        project.timeline=Some(serde_json::from_value(serde_json::json!({"segments":[{"recordingSegment":1,"start":0.0,"end":5.0,"timescale":2.0}],"zoomSegments":[]})).unwrap());
        synchronize_captions(&mut project, &[5.0, 5.0]);
        assert_eq!(
            project.timeline.as_ref().unwrap().caption_segments[0].start,
            0.5
        );
        assert_eq!(
            project.timeline.as_ref().unwrap().caption_segments[0].end,
            1.0
        );
    }

    #[test]
    fn unsorted_words_outside_caption_bounds_keep_both_clip_projections() {
        let timeline = project(true, 0.0, 0.5).timeline.unwrap();
        let captions = [CaptionSegment {
            manual_timing: None,
            id: "unsorted".into(),
            start: 0.0,
            end: 0.5,
            text: "parent".into(),
            words: vec![
                CaptionWord {
                    text: "late-first".into(),
                    start: 4.0,
                    end: 4.5,
                    hidden: false,
                },
                CaptionWord {
                    text: "second-clip".into(),
                    start: 7.0,
                    end: 7.5,
                    hidden: false,
                },
                CaptionWord {
                    text: "early-last".into(),
                    start: 1.0,
                    end: 1.5,
                    hidden: false,
                },
            ],
        }];

        let track = derive_caption_track_segments(&captions, &timeline, &[10.0]);
        assert_eq!(track.len(), 2);
        assert_eq!(track[0].id, "unsorted::edl0");
        assert_eq!(track[0].start, 4.0);
        assert_eq!(track[0].end, 1.5);
        assert_eq!(track[0].text, "late-first early-last");
        assert_eq!(
            track[0]
                .words
                .iter()
                .map(|word| (word.text.as_str(), word.start, word.end))
                .collect::<Vec<_>>(),
            vec![("late-first", 4.0, 4.5), ("early-last", 1.0, 1.5)]
        );
        assert_eq!(track[1].id, "unsorted::edl1");
        assert_eq!(track[1].start, 6.0);
        assert_eq!(track[1].end, 6.5);
        assert_eq!(track[1].text, "second-clip");
        assert_eq!(
            track[1]
                .words
                .iter()
                .map(|word| (word.text.as_str(), word.start, word.end))
                .collect::<Vec<_>>(),
            vec![("second-clip", 6.0, 6.5)]
        );
    }

    #[test]
    fn nan_word_timestamp_keeps_the_existing_mapping_path() {
        let timeline = project(true, 0.0, 0.5).timeline.unwrap();
        let captions = [CaptionSegment {
            manual_timing: None,
            id: "nan".into(),
            start: 0.0,
            end: 0.5,
            text: "parent".into(),
            words: vec![
                CaptionWord {
                    text: "nan-start".into(),
                    start: f32::NAN,
                    end: 1.0,
                    hidden: false,
                },
                CaptionWord {
                    text: "second-clip".into(),
                    start: 7.0,
                    end: 7.5,
                    hidden: false,
                },
            ],
        }];

        let track = derive_caption_track_segments(&captions, &timeline, &[10.0]);
        assert_eq!(track.len(), 2);
        assert_eq!(track[0].id, "nan::edl0");
        assert_eq!(track[0].start, 0.0);
        assert_eq!(track[0].end, 1.0);
        assert_eq!(track[0].text, "nan-start");
        assert_eq!(
            track[0]
                .words
                .iter()
                .map(|word| (word.text.as_str(), word.start, word.end))
                .collect::<Vec<_>>(),
            vec![("nan-start", 0.0, 1.0)]
        );
        assert_eq!(track[1].id, "nan::edl1");
        assert_eq!(track[1].start, 6.0);
        assert_eq!(track[1].end, 6.5);
        assert_eq!(track[1].text, "second-clip");
        assert_eq!(
            track[1]
                .words
                .iter()
                .map(|word| (word.text.as_str(), word.start, word.end))
                .collect::<Vec<_>>(),
            vec![("second-clip", 6.0, 6.5)]
        );
    }
}
