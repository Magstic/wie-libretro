use alloc::{boxed::Box, collections::BTreeMap, collections::BTreeSet, sync::Arc, vec, vec::Vec};

use smaf_player::{SmafEvent, parse_smaf};

use crate::{AudioCommand, AudioEventData, AudioHandle, AudioSequence, AudioSink, TimedAudioEvent};

#[derive(Debug)]
pub enum AudioError {
    InvalidHandle,
}

pub struct Audio {
    sink: Box<dyn AudioSink>,
    files: BTreeMap<AudioHandle, Arc<AudioSequence>>,
    playing: BTreeSet<AudioHandle>,
    last_audio_handle: AudioHandle,
}

impl Audio {
    pub fn new(sink: Box<dyn AudioSink>) -> Self {
        Self {
            sink,
            files: BTreeMap::new(),
            playing: BTreeSet::new(),
            last_audio_handle: 0,
        }
    }

    pub fn load_smaf(&mut self, data: &[u8]) -> Result<AudioHandle, AudioError> {
        let audio_handle = self.last_audio_handle;
        let sequence = Arc::new(convert_smaf_events(parse_smaf(data)));

        self.last_audio_handle += 1;
        self.files.insert(audio_handle, sequence);

        Ok(audio_handle)
    }

    pub fn play(&mut self, audio_handle: AudioHandle, repeat: bool) -> Result<(), AudioError> {
        let sequence = self.files.get(&audio_handle).cloned().ok_or(AudioError::InvalidHandle)?;

        self.stop(audio_handle);
        self.playing.insert(audio_handle);
        self.sink.send(AudioCommand::Play {
            handle: audio_handle,
            sequence,
            repeat,
        });

        Ok(())
    }

    pub fn stop(&mut self, audio_handle: AudioHandle) {
        if self.playing.remove(&audio_handle) {
            self.sink.send(AudioCommand::Stop { handle: audio_handle });
        }
    }

    pub fn close(&mut self, audio_handle: AudioHandle) -> Result<(), AudioError> {
        self.stop(audio_handle);

        if self.files.remove(&audio_handle).is_none() {
            return Err(AudioError::InvalidHandle);
        }

        Ok(())
    }

    pub fn shutdown(&mut self) {
        let playing = core::mem::take(&mut self.playing);

        for handle in playing {
            self.sink.send(AudioCommand::Stop { handle });
        }

        self.files.clear();
    }
}

fn convert_smaf_events(events: Vec<(usize, SmafEvent)>) -> AudioSequence {
    // ZIC2's sampled material is <= 16 kHz and the MA FM model is intentionally
    // low-passed near 10 kHz. 24 kHz therefore preserves the useful band while
    // cutting FM synthesis work by ~46% versus 44.1 kHz.
    const EMBEDDED_SAMPLE_RATE: u32 = 24_000;

    // parse_smaf() emits the authoritative file-level End after Wave/FM tails.
    let duration = events.iter().map(|(time, _)| *time as u64).max().unwrap_or(0);

    // Keep file-authored audio as a compact event sequence. Each output backend
    // decides how to consume it; real-time backends must not synthesize a whole
    // song while the guest is blocked in load_smaf().
    let mut embedded = Vec::new();
    let mut transport_events: Vec<TimedAudioEvent> = events
        .into_iter()
        .filter_map(|(time, event)| {
            let data = match event {
                event @ (SmafEvent::Wave { .. } | SmafEvent::MaFmNote(_)) => {
                    embedded.push((time, event));
                    return None;
                }
                SmafEvent::End => {
                    return None;
                }
                SmafEvent::MidiNoteOn { channel, note, velocity } => AudioEventData::Midi(vec![0x90 | channel, note, velocity]),
                SmafEvent::MidiNoteOff { channel, note, velocity } => AudioEventData::Midi(vec![0x80 | channel, note, velocity]),
                SmafEvent::MidiProgramChange { channel, program } => AudioEventData::Midi(vec![0xc0 | channel, program]),
                SmafEvent::MidiControlChange { channel, control, value } => AudioEventData::Midi(vec![0xb0 | channel, control, value]),
                SmafEvent::MidiPitchBend { channel, value } => {
                    AudioEventData::Midi(vec![0xe0 | channel, (value & 0x7f) as u8, ((value >> 7) & 0x7f) as u8])
                }
                SmafEvent::MidiSysEx(data) => AudioEventData::Midi(data),
            };

            Some(TimedAudioEvent { time: time as u64, data })
        })
        .collect();

    if !embedded.is_empty() {
        transport_events.push(TimedAudioEvent {
            time: 0,
            data: AudioEventData::Smaf {
                sampling_rate: EMBEDDED_SAMPLE_RATE,
                events: Arc::new(embedded),
            },
        });
    }

    // The embedded sequence was inserted after collecting the MIDI events.
    transport_events.sort_by_key(|event| event.time);

    AudioSequence {
        duration,
        events: transport_events,
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use alloc::{boxed::Box, sync::Arc, vec, vec::Vec};
    use std::sync::Mutex;

    use smaf_player::{SmafEvent, WaveDynamics};

    use super::{Audio, convert_smaf_events};
    use crate::{AudioCommand, AudioEventData, AudioSequence, AudioSink, TimedAudioEvent};

    struct RecordingSink(Arc<Mutex<Vec<AudioCommand>>>);

    impl AudioSink for RecordingSink {
        fn send(&self, command: AudioCommand) {
            self.0.lock().unwrap().push(command);
        }
    }

    #[test]
    fn converts_smaf_events_to_timed_transport() {
        let sequence = convert_smaf_events(vec![
            (5, SmafEvent::MidiProgramChange { channel: 2, program: 7 }),
            (10, SmafEvent::MidiPitchBend { channel: 3, value: 0x1234 }),
            (15, SmafEvent::End),
        ]);

        assert_eq!(
            sequence,
            AudioSequence {
                duration: 15,
                events: vec![
                    TimedAudioEvent {
                        time: 5,
                        data: AudioEventData::Midi(vec![0xc2, 7]),
                    },
                    TimedAudioEvent {
                        time: 10,
                        data: AudioEventData::Midi(vec![0xe3, 0x34, 0x24]),
                    },
                ],
            }
        );
    }

    #[test]
    fn keeps_embedded_smaf_as_lazy_events_instead_of_a_song_sized_wave() {
        let sequence = convert_smaf_events(vec![
            (
                12,
                SmafEvent::Wave {
                    channels: 1,
                    sampling_rate: 8_000,
                    data: vec![1, 2, 3],
                    dynamics: WaveDynamics::UNITY,
                },
            ),
            (20, SmafEvent::End),
        ]);

        let AudioEventData::Smaf { sampling_rate, events } = &sequence.events[0].data else {
            panic!("expected lazy SMAF event sequence");
        };
        assert_eq!(*sampling_rate, 24_000);
        assert!(matches!(events.as_slice(), [(12, SmafEvent::Wave { .. })]));
        assert!(!sequence.events.iter().any(|event| matches!(&event.data, AudioEventData::Wave { .. })));
    }

    #[test]
    fn replay_stops_previous_playback_before_starting_again() {
        let commands = Arc::new(Mutex::new(Vec::new()));
        let mut audio = Audio::new(Box::new(RecordingSink(commands.clone())));
        let handle = audio.load_smaf(&[]).unwrap();

        audio.play(handle, false).unwrap();
        audio.play(handle, true).unwrap();

        let commands = commands.lock().unwrap();
        let AudioCommand::Play {
            sequence: first_sequence,
            repeat: false,
            ..
        } = &commands[0]
        else {
            panic!("expected initial play command");
        };
        assert_eq!(commands[1], AudioCommand::Stop { handle });
        let AudioCommand::Play {
            sequence: second_sequence,
            repeat: true,
            ..
        } = &commands[2]
        else {
            panic!("expected replay command");
        };
        assert!(Arc::ptr_eq(first_sequence, second_sequence));
    }

    #[test]
    fn close_stops_playback_and_removes_the_handle() {
        let commands = Arc::new(Mutex::new(Vec::new()));
        let mut audio = Audio::new(Box::new(RecordingSink(commands.clone())));
        let handle = audio.load_smaf(&[]).unwrap();

        audio.play(handle, false).unwrap();
        audio.close(handle).unwrap();

        assert_eq!(commands.lock().unwrap()[1], AudioCommand::Stop { handle });
        assert!(audio.play(handle, false).is_err());
    }
}
