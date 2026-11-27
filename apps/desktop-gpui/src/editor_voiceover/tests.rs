//! [INPUT]: 配音状态和临时工程配置。
//! [OUTPUT]: 配音位置、恢复和锁定回归。
//! [POS]: 验证编辑器配音状态转换。
//! [PROTOCOL]: Update this header on change, then check AGENTS.md

use super::*;

#[test]
fn voiceover_placement_uses_output_seconds_and_reserves_an_unoccupied_lane() {
    let project: cap_project::ProjectConfiguration = serde_json::from_value(serde_json::json!({
        "timeline": { "segments": [{"recordingSegment":0,"start":0.,"end":20.,"timescale":1.25}], "zoomSegments": [],
            "audioSegments": [{"start":8.,"end":12.,"track":1,"path":"assets/audio/music.mp3"}] }
    })).unwrap();
    let plan = placement(&project, 5.25, 16., 1, 2).unwrap();
    assert_eq!(
        plan,
        Placement {
            start: 5.25,
            lane: 2,
        }
    );
    assert_eq!(placement(&project, 16., 16., 1, 2).unwrap().start, 16.);
    assert!(placement(&project, f64::NAN, 16., 1, 2).is_none());
    assert_eq!(placement(&project, 12., 16., 1, 2).unwrap().lane, 1);
}

#[test]
fn voiceover_finalization_and_recovery_keep_the_project_locked() {
    for phase in [
        Phase::Starting,
        Phase::Recording,
        Phase::Paused,
        Phase::Finalizing,
        Phase::Recovering,
    ] {
        assert!(phase.busy());
    }
    for phase in [Phase::Idle, Phase::Ready, Phase::Error] {
        assert!(!phase.busy());
    }
}
#[test]
fn voiceover_source_finalization_remains_locked_and_observed_while_user_stop_owns_its_reply() {
    let mut phase = Phase::Recording;
    assert_eq!(
        observe_recording_phase(&mut phase, TakeState::Finalizing),
        StatusAction::Continue
    );
    assert_eq!(phase, Phase::SourceFinalizing);
    assert!(phase.busy());
    assert_eq!(
        observe_recording_phase(&mut phase, TakeState::Ready),
        StatusAction::Recover
    );
    assert_eq!(phase, Phase::Ready);
    let mut phase = Phase::Paused;
    assert_eq!(
        observe_recording_phase(&mut phase, TakeState::Finalizing),
        StatusAction::Continue
    );
    assert_eq!(
        observe_recording_phase(&mut phase, TakeState::Failed),
        StatusAction::Failed
    );
    assert_eq!(phase, Phase::Error);
    let mut phase = Phase::Finalizing;
    assert_eq!(
        observe_recording_phase(&mut phase, TakeState::Ready),
        StatusAction::Ignore
    );
    assert_eq!(phase, Phase::Finalizing);
    assert!(phase.busy());
}

#[test]
fn voiceover_recovery_moves_off_occupied_lane_and_save_retry_keeps_existing_clip() {
    let mut project: cap_project::ProjectConfiguration = serde_json::from_value(serde_json::json!({
        "timeline": { "segments": [{"recordingSegment":0,"start":0.,"end":20.,"timescale":1.25}], "zoomSegments": [],
            "audioSegments": [{"start":0.,"end":16.,"track":1,"path":"assets/audio/music.mp3"}] }
    })).unwrap();
    let path = "content/voiceovers/recovery/capture.m4a";
    let (lane, range) = audio_destination(&project, path, 1, 2, 5.25, 16., 2.).unwrap();
    assert_eq!((lane, range.clone()), (2, 5.25..7.25));
    project.timeline.as_mut().unwrap().audio_segments.push(
        crate::editor_edits::default_audio_segment(
            range.start,
            range.end,
            lane,
            path.into(),
            "Voiceover".into(),
            Some(2.),
        ),
    );
    let segment = project
        .timeline
        .as_mut()
        .unwrap()
        .audio_segments
        .last_mut()
        .unwrap();
    segment.track = 4;
    segment.start = 8.;
    segment.end = 10.;
    let snapshot = serde_json::to_value(&project).unwrap();
    assert_eq!(
        audio_destination(&project, path, 1, 5, 5.25, 16., 2.).unwrap(),
        (4, 8.0..10.0)
    );
    assert_eq!(serde_json::to_value(&project).unwrap(), snapshot);
    assert_eq!(project.timeline.as_ref().unwrap().audio_segments.len(), 2);
}
