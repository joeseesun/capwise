use block2::RcBlock;
use objc2::{
    AnyThread,
    rc::{Retained, autoreleasepool},
};
use objc2_avf_audio::{AVAudioFormat, AVAudioPCMBuffer};
use objc2_foundation::{NSBundle, NSError, NSLocale, NSString, ns_string};
use objc2_speech::{
    SFSpeechAudioBufferRecognitionRequest, SFSpeechRecognitionResult, SFSpeechRecognitionTask,
    SFSpeechRecognitionTaskHint, SFSpeechRecognizer,
    SFSpeechRecognizerAuthorizationStatus as Authorization,
};

use super::{
    Duration, Failure, FollowGeneration, FollowPhase, Instant, MicPacket, MicTapEpoch,
    MicTapStatus, RecognitionPacket, SessionEvent, StreamingEvent, cancelled_microphone_status,
    mpsc, oneshot, wait_for_microphone, watch,
};
use crate::{mic_tap::SAMPLE_RATE, teleprompter_follow::RecognitionUpdate};

const TASK_DURATION: Duration = Duration::from_secs(55);
const UNAVAILABLE: FollowPhase = FollowPhase::Failed(Failure::SpeechUnavailable);
const INTERRUPTED: FollowPhase = FollowPhase::Failed(Failure::Service);

pub(super) struct Source {
    locale: String,
    authorization: Option<oneshot::Receiver<Authorization>>,
}

fn speech_usage_declared() -> bool {
    NSBundle::mainBundle()
        .objectForInfoDictionaryKey(ns_string!("NSSpeechRecognitionUsageDescription"))
        .is_some()
}

fn local_recognizer(locale: &str) -> Option<Retained<SFSpeechRecognizer>> {
    autoreleasepool(|_| unsafe {
        let locale = NSLocale::localeWithLocaleIdentifier(&NSString::from_str(locale));
        SFSpeechRecognizer::initWithLocale(SFSpeechRecognizer::alloc(), &locale)
            .filter(|recognizer| recognizer.supportsOnDeviceRecognition())
    })
}

pub(super) fn supports_local_follow(script: &str) -> bool {
    speech_usage_declared() && local_recognizer(&locale_for_script(script, 0)).is_some()
}

pub(super) fn can_auto_start() -> bool {
    unsafe { SFSpeechRecognizer::authorizationStatus() != Authorization::NotDetermined }
}

pub(super) fn authorize(script: &str, offset: usize) -> Result<Source, FollowPhase> {
    if !speech_usage_declared() {
        return Err(UNAVAILABLE);
    }
    let locale = locale_for_script(script, offset);
    if local_recognizer(&locale).is_none() {
        return Err(UNAVAILABLE);
    }
    let authorization = match unsafe { SFSpeechRecognizer::authorizationStatus() } {
        Authorization::Authorized => None,
        Authorization::NotDetermined => {
            let (sender, receiver) = oneshot::channel();
            let sender = std::sync::Mutex::new(Some(sender));
            let callback = RcBlock::new(move |status| {
                if let Ok(mut sender) = sender.lock()
                    && let Some(sender) = sender.take()
                {
                    let _ = sender.send(status);
                }
            });
            unsafe { SFSpeechRecognizer::requestAuthorization(&callback) };
            Some(receiver)
        }
        _ => return Err(FollowPhase::Failed(Failure::Permission)),
    };
    Ok(Source {
        locale,
        authorization,
    })
}

fn locale_for_script(script: &str, offset: usize) -> String {
    let preferred = NSLocale::currentLocale().localeIdentifier().to_string();
    let text = script.get(offset..).unwrap_or(script);
    speech_locale(
        if text.trim().is_empty() { script } else { text },
        &preferred,
    )
}

fn speech_locale(text: &str, preferred: &str) -> String {
    let mut han = 0;
    let mut latin = 0;
    for c in text.chars().filter(|c| c.is_alphabetic()).take(160) {
        match c as u32 {
            0x3040..=0x30ff => return "ja-JP".into(),
            0x1100..=0x11ff | 0x3130..=0x318f | 0xac00..=0xd7af => return "ko-KR".into(),
            0x3400..=0x9fff | 0xf900..=0xfaff | 0x20000..=0x323af => han += 1,
            _ if c.is_ascii_alphabetic() => latin += 1,
            _ => {}
        }
    }
    if han > 0 && han >= latin {
        if preferred.starts_with("zh") {
            return preferred.into();
        }
        return "zh-CN".into();
    }
    if latin > 0
        && ["zh", "ja", "ko"]
            .iter()
            .any(|code| preferred.starts_with(code))
    {
        return "en-US".into();
    }
    preferred.into()
}

pub(super) async fn drive_session(
    generation: FollowGeneration,
    mut source: Source,
    mut status: watch::Receiver<MicTapStatus>,
    packets: flume::Receiver<MicPacket>,
    updates: mpsc::Sender<SessionEvent>,
) -> Result<(), FollowPhase> {
    if let Some(authorization) = source.authorization.take()
        && super::discard_until(authorization, &packets).await?.ok()
            != Some(Authorization::Authorized)
    {
        return Err(FollowPhase::Failed(Failure::Permission));
    }
    let epoch = wait_for_microphone(&mut status, &packets).await?;
    let (mut done, result) = oneshot::channel();
    let runtime = tokio::runtime::Handle::current();
    std::thread::Builder::new()
        .name("teleprompter-speech".into())
        .spawn(move || {
            runtime.block_on(async {
                let result = tokio::select! {
                    biased;
                    _ = done.closed() => return,
                    _ = epoch.cancelled() => Err(cancelled_microphone_status(&status.borrow(), epoch.generation())),
                    result = listen(generation, &source.locale, &epoch, &packets, &updates) => result,
                };
                let _ = done.send(result);
            });
        })
        .map_err(|_| INTERRUPTED)?;
    result.await.map_err(|_| INTERRUPTED)?
}

#[derive(Clone)]
enum SpeechEvent {
    Recognition(RecognitionUpdate),
    Restart,
    Retry,
    Failed(FollowPhase),
}

impl SpeechEvent {
    fn is_terminal(&self) -> bool {
        !matches!(
            self,
            Self::Recognition(RecognitionUpdate {
                definite: false,
                ..
            })
        )
    }
}

struct Task {
    request: Retained<SFSpeechAudioBufferRecognitionRequest>,
    task: Retained<SFSpeechRecognitionTask>,
    events: watch::Receiver<Option<SpeechEvent>>,
}

impl Task {
    fn start(recognizer: &SFSpeechRecognizer, origin: Instant) -> Self {
        let start_ms = origin.elapsed().as_millis() as u64;
        let (send, events) = watch::channel(None);
        unsafe {
            let request = SFSpeechAudioBufferRecognitionRequest::new();
            request.setRequiresOnDeviceRecognition(true);
            request.setShouldReportPartialResults(true);
            request.setTaskHint(SFSpeechRecognitionTaskHint::Dictation);
            let handler = RcBlock::new(
                move |result: *mut SFSpeechRecognitionResult, error: *mut NSError| {
                    let update = result.as_ref().map(|result| RecognitionUpdate {
                        start_ms,
                        end_ms: origin.elapsed().as_millis() as u64,
                        text: result.bestTranscription().formattedString().to_string(),
                        definite: result.isFinal(),
                    });
                    let event = match (update, error.as_ref()) {
                        (Some(update), _) if update.definite => SpeechEvent::Recognition(update),
                        (_, Some(error)) => {
                            let domain = error.domain();
                            let code = error.code();
                            tracing::debug!(%domain, code, "Apple Speech task ended");
                            if domain.isEqualToString(ns_string!("kAFAssistantErrorDomain")) {
                                match code {
                                    1110 => SpeechEvent::Restart,
                                    1100 | 1101 | 1107 => SpeechEvent::Retry,
                                    1700 => SpeechEvent::Failed(FollowPhase::Failed(
                                        Failure::Permission,
                                    )),
                                    _ => SpeechEvent::Failed(INTERRUPTED),
                                }
                            } else if domain.isEqualToString(ns_string!("kLSRErrorDomain"))
                                && matches!(code, 102 | 201)
                            {
                                SpeechEvent::Failed(UNAVAILABLE)
                            } else {
                                SpeechEvent::Failed(INTERRUPTED)
                            }
                        }
                        (Some(update), None) => SpeechEvent::Recognition(update),
                        (None, None) => return,
                    };
                    send.send_if_modified(|current| {
                        if current.as_ref().is_some_and(SpeechEvent::is_terminal) {
                            return false;
                        }
                        *current = Some(event);
                        true
                    });
                },
            );
            let task = recognizer.recognitionTaskWithRequest_resultHandler(&request, &handler);
            Self {
                request,
                task,
                events,
            }
        }
    }

    fn append(&self, format: &AVAudioFormat, samples: &[i16]) -> Result<(), FollowPhase> {
        if samples.is_empty() {
            return Ok(());
        }
        autoreleasepool(|_| unsafe {
            let buffer = speech_buffer(format, samples).ok_or(INTERRUPTED)?;
            self.request.appendAudioPCMBuffer(&buffer);
            Ok(())
        })
    }
}

fn speech_format() -> Option<Retained<AVAudioFormat>> {
    unsafe {
        AVAudioFormat::initStandardFormatWithSampleRate_channels(
            AVAudioFormat::alloc(),
            f64::from(SAMPLE_RATE),
            1,
        )
    }
}

fn speech_buffer(format: &AVAudioFormat, samples: &[i16]) -> Option<Retained<AVAudioPCMBuffer>> {
    unsafe {
        let length = u32::try_from(samples.len()).ok()?;
        let buffer = AVAudioPCMBuffer::initWithPCMFormat_frameCapacity(
            AVAudioPCMBuffer::alloc(),
            format,
            length,
        )?;
        buffer.setFrameLength(length);
        let channel = buffer.floatChannelData().as_ref()?;
        let destination = std::slice::from_raw_parts_mut(channel.as_ptr(), samples.len());
        for (destination, &sample) in destination.iter_mut().zip(samples) {
            *destination = f32::from(sample) / 32768.;
        }
        Some(buffer)
    }
}

impl Drop for Task {
    fn drop(&mut self) {
        unsafe { self.task.cancel() };
    }
}

async fn listen(
    generation: FollowGeneration,
    locale: &str,
    epoch: &MicTapEpoch,
    packets: &flume::Receiver<MicPacket>,
    updates: &mpsc::Sender<SessionEvent>,
) -> Result<(), FollowPhase> {
    let recognizer = local_recognizer(locale).ok_or(UNAVAILABLE)?;
    let format = autoreleasepool(|_| speech_format()).ok_or(INTERRUPTED)?;
    let origin = Instant::now();
    let mut announced = false;
    let mut retries = 0;
    loop {
        if !unsafe { recognizer.isAvailable() } {
            return Err(UNAVAILABLE);
        }
        let mut task = autoreleasepool(|_| Task::start(&recognizer, origin));
        let ready = Instant::now();
        let renew = tokio::time::sleep(TASK_DURATION);
        tokio::pin!(renew);
        if !announced {
            updates
                .send(SessionEvent {
                    generation,
                    event: StreamingEvent::Ready,
                })
                .await
                .map_err(|_| INTERRUPTED)?;
            announced = true;
        }
        loop {
            tokio::select! {
                biased;
                _ = updates.closed() => return Ok(()),
                _ = &mut renew => break,
                changed = task.events.changed() => {
                    changed.map_err(|_| INTERRUPTED)?;
                    let Some(event) = task.events.borrow_and_update().clone() else { continue; };
                    match event {
                        SpeechEvent::Recognition(update) => {
                            retries = 0;
                            let final_packet = update.definite;
                            let event = SessionEvent {
                                generation,
                                event: StreamingEvent::Recognition(RecognitionPacket {
                                    sequence: None,
                                    final_packet,
                                    updates: vec![update],
                                }),
                            };
                            if final_packet {
                                updates.send(event).await.map_err(|_| INTERRUPTED)?;
                                break;
                            }
                            if let Err(mpsc::error::TrySendError::Closed(_)) = updates.try_send(event) {
                                return Ok(());
                            }
                        }
                        SpeechEvent::Restart if ready.elapsed() >= Duration::from_secs(1) => break,
                        SpeechEvent::Retry if retries < 2 => {
                            retries += 1;
                            drop(task);
                            tokio::time::sleep(Duration::from_millis(300)).await;
                            break;
                        }
                        SpeechEvent::Restart | SpeechEvent::Retry => return Err(INTERRUPTED),
                        SpeechEvent::Failed(phase) => return Err(phase),
                    }
                }
                packet = packets.recv_async() => {
                    let packet = packet.map_err(|_| FollowPhase::Failed(Failure::Microphone))?;
                    if packet.epoch.generation() == epoch.generation()
                        && packet.captured_at >= origin
                    {
                        if !packet.is_current() {
                            return Err(FollowPhase::Failed(Failure::AudioLag));
                        }
                        task.append(&format, &packet.samples)?;
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apple_speech_buffer_has_the_microphone_format_and_normalized_samples() {
        autoreleasepool(|_| {
            let format = speech_format().unwrap();
            let buffer = speech_buffer(&format, &[-32768, -16384, 0, 16384]).unwrap();
            unsafe {
                assert_eq!(format.channelCount(), 1);
                assert_eq!(format.sampleRate(), f64::from(SAMPLE_RATE));
                assert_eq!(buffer.frameLength(), 4);
                let channel = buffer.floatChannelData().as_ref().unwrap();
                assert_eq!(
                    std::slice::from_raw_parts(channel.as_ptr(), 4),
                    &[-1., -0.5, 0., 0.5]
                );
            }
        });
    }

    #[test]
    fn apple_speech_uses_the_script_language_over_unrelated_system_preferences() {
        assert_eq!(speech_locale("接下来开始录屏", "en_US"), "zh-CN");
        assert_eq!(speech_locale("接下來開始錄影", "zh_TW"), "zh_TW");
        assert_eq!(speech_locale("Start recording now.", "zh_CN"), "en-US");
        assert_eq!(speech_locale("Bonjour et bienvenue.", "fr_FR"), "fr_FR");
        assert_eq!(speech_locale("録画を始めます。", "zh_CN"), "ja-JP");
    }
}
