use alloc::{
    boxed::Box,
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
    vec::Vec,
};
use core::sync::atomic::{AtomicBool, Ordering};

use smaf_player::{SmafEvent, parse_smaf};
use spin::Mutex;

use crate::{System, audio_sink::AudioSink};

pub type AudioHandle = u32;
#[derive(Debug)]
pub enum AudioError {
    InvalidHandle,
    InvalidAudio,
}

enum AudioFile {
    Smaf(Vec<u8>),
}

pub struct Audio {
    sink: Arc<Box<dyn AudioSink>>,
    midi_channels: Arc<Mutex<MidiChannelAllocator>>,
    files: BTreeMap<AudioHandle, AudioFile>,
    playing: BTreeMap<AudioHandle, Arc<AtomicBool>>,
    last_audio_handle: AudioHandle,
}

impl Audio {
    pub fn new(sink: Box<dyn AudioSink>) -> Self {
        Self {
            sink: Arc::new(sink),
            midi_channels: Arc::new(Mutex::new(MidiChannelAllocator::default())),
            files: BTreeMap::new(),
            playing: BTreeMap::new(),
            last_audio_handle: 0,
        }
    }

    pub fn load_smaf(&mut self, data: &[u8]) -> Result<AudioHandle, AudioError> {
        let audio_handle = self.last_audio_handle;

        self.last_audio_handle += 1;
        self.files.insert(audio_handle, AudioFile::Smaf(data.to_vec()));

        Ok(audio_handle)
    }

    pub fn play(&mut self, system: &System, audio_handle: AudioHandle) -> Result<(), AudioError> {
        self.play_with_loop_count(system, audio_handle, 1)
    }

    pub fn play_repeated(&mut self, system: &System, audio_handle: AudioHandle, repeat: bool) -> Result<(), AudioError> {
        self.play_with_loop_count(system, audio_handle, if repeat { -1 } else { 1 })
    }

    pub fn play_with_loop_count(&mut self, system: &System, audio_handle: AudioHandle, loop_count: i32) -> Result<(), AudioError> {
        let player = match self.files.get(&audio_handle) {
            Some(AudioFile::Smaf(data)) => SmafPlayer::new(data),
            None => return Err(AudioError::InvalidHandle),
        };

        self.stop(audio_handle);

        let mut system_clone = system.clone();
        let sink_clone = self.sink.clone();
        let midi_channel_map = MidiChannelMap::allocate(self.midi_channels.clone(), &player.used_midi_channels());

        let stop_flag = Arc::new(AtomicBool::new(false));
        let stop_flag_clone = stop_flag.clone();
        self.playing.insert(audio_handle, stop_flag);

        let loop_count = normalize_loop_count(loop_count);

        // TODO use dedicated audio player task
        system.spawn(async move || {
            let mut remaining = loop_count;

            loop {
                player.play_once(&mut system_clone, &**sink_clone, &stop_flag_clone, &midi_channel_map).await;

                if stop_flag_clone.load(Ordering::Relaxed) || !player.can_repeat() {
                    break;
                }

                if let Some(count) = remaining.as_mut() {
                    *count = count.saturating_sub(1);
                    if *count == 0 {
                        break;
                    }
                }
            }

            Ok(())
        });

        Ok(())
    }

    pub fn stop(&mut self, audio_handle: AudioHandle) {
        if let Some(stop_flag) = self.playing.remove(&audio_handle) {
            stop_flag.store(true, Ordering::Relaxed);
        }
    }

    pub fn close(&mut self, audio_handle: AudioHandle) -> Result<(), AudioError> {
        self.stop(audio_handle);

        if self.files.remove(&audio_handle).is_none() {
            return Err(AudioError::InvalidHandle);
        }

        Ok(())
    }
}

pub struct SmafPlayer {
    events: Vec<(usize, SmafEvent)>,
}

impl SmafPlayer {
    pub fn new(data: &[u8]) -> Self {
        Self { events: parse_smaf(data) }
    }

    fn used_midi_channels(&self) -> BTreeSet<u8> {
        self.events
            .iter()
            .filter_map(|(_, event)| match event {
                SmafEvent::MidiNoteOn { channel, .. }
                | SmafEvent::MidiNoteOff { channel, .. }
                | SmafEvent::MidiProgramChange { channel, .. }
                | SmafEvent::MidiControlChange { channel, .. }
                | SmafEvent::MidiPitchBend { channel, .. } => Some(channel & 0x0f),
                SmafEvent::Wave { .. } | SmafEvent::MidiSysEx(_) | SmafEvent::End => None,
            })
            .collect()
    }

    fn can_repeat(&self) -> bool {
        self.events.iter().any(|(time, event)| *time > 0 || !matches!(event, SmafEvent::End))
    }

    async fn play_once(&self, system: &mut System, sink: &dyn AudioSink, stop_flag: &AtomicBool, midi_channel_map: &MidiChannelMap) {
        let mut active_notes: Vec<(u8, u8)> = Vec::new();
        let mut used_channels: BTreeSet<u8> = BTreeSet::new();

        let start_time = system.platform().now();
        for (time, event) in &self.events {
            if stop_flag.load(Ordering::Relaxed) {
                break;
            }

            let now = system.platform().now();
            if (*time as u64) > now - start_time {
                system.sleep(((*time as u64) - (now - start_time)) as _).await;

                if stop_flag.load(Ordering::Relaxed) {
                    break;
                }
            }

            match event {
                SmafEvent::Wave {
                    channel,
                    sampling_rate,
                    data,
                } => {
                    sink.play_wave(*channel, *sampling_rate, data);
                }
                SmafEvent::MidiNoteOn { channel, note, velocity } => {
                    let channel = midi_channel_map.map(*channel);
                    sink.midi_note_on(channel, *note, *velocity);
                    active_notes.push((channel, *note));
                    used_channels.insert(channel);
                }
                SmafEvent::MidiNoteOff { channel, note, velocity } => {
                    let channel = midi_channel_map.map(*channel);
                    sink.midi_note_off(channel, *note, *velocity);
                    active_notes.retain(|(c, n)| !(*c == channel && *n == *note));
                }
                SmafEvent::MidiProgramChange { channel, program } => {
                    let channel = midi_channel_map.map(*channel);
                    sink.midi_program_change(channel, *program);
                    used_channels.insert(channel);
                }
                SmafEvent::MidiControlChange { channel, control, value } => {
                    let channel = midi_channel_map.map(*channel);
                    sink.midi_control_change(channel, *control, *value);
                    used_channels.insert(channel);
                }
                SmafEvent::MidiPitchBend { channel, value } => {
                    let channel = midi_channel_map.map(*channel);
                    sink.midi_pitch_bend(channel, *value);
                    used_channels.insert(channel);
                }
                SmafEvent::MidiSysEx(data) => {
                    sink.midi_sysex(data);
                }
                SmafEvent::End => {}
            }
        }

        for (channel, note) in &active_notes {
            sink.midi_note_off(*channel, *note, 0);
        }

        // Release sustain and force any lingering voices off on every channel
        // this track touched. Tracks that set sustain pedal (CC 64) or use
        // long release envelopes (e.g. drum voices) otherwise keep ringing
        // after note_off.
        for channel in &used_channels {
            sink.midi_control_change(*channel, 64, 0); // sustain off
            sink.midi_control_change(*channel, 120, 0); // all sound off
            sink.midi_control_change(*channel, 123, 0); // all notes off
        }
    }
}


fn normalize_loop_count(loop_count: i32) -> Option<u32> {
    match loop_count {
        0 => Some(1),
        x if x < 0 => None,
        x => Some(x as u32),
    }
}

#[derive(Default)]
struct MidiChannelAllocator {
    used: [bool; 16],
}

impl MidiChannelAllocator {
    fn reserve(&mut self, channel: u8) -> bool {
        let channel = usize::from(channel);
        if self.used[channel] {
            return false;
        }

        self.used[channel] = true;
        true
    }

    fn release(&mut self, channel: u8) {
        self.used[usize::from(channel)] = false;
    }

    fn first_free_melodic_channel(&self) -> Option<u8> {
        (0..16).find(|channel| *channel != 9 && !self.used[*channel]).map(|channel| channel as _)
    }
}

struct MidiChannelMap {
    allocator: Arc<Mutex<MidiChannelAllocator>>,
    allocated_channels: Vec<u8>,
    mapped_channels: BTreeMap<u8, u8>,
}

impl MidiChannelMap {
    fn allocate(allocator: Arc<Mutex<MidiChannelAllocator>>, source_channels: &BTreeSet<u8>) -> Self {
        let mut mapped_channels = BTreeMap::new();
        let mut allocated_channels = Vec::new();

        {
            let mut allocator_guard = allocator.lock();
            for source in source_channels {
                let source = source & 0x0f;
                if mapped_channels.contains_key(&source) {
                    continue;
                }

                if allocator_guard.reserve(source) {
                    mapped_channels.insert(source, source);
                    allocated_channels.push(source);
                    continue;
                }

                let destination = if source == 9 {
                    None
                } else {
                    allocator_guard.first_free_melodic_channel()
                };

                if let Some(destination) = destination {
                    allocator_guard.reserve(destination);
                    mapped_channels.insert(source, destination);
                    allocated_channels.push(destination);
                } else {
                    mapped_channels.insert(source, source);
                }
            }
        }

        Self {
            allocator,
            allocated_channels,
            mapped_channels,
        }
    }

    fn map(&self, channel: u8) -> u8 {
        let channel = channel & 0x0f;
        self.mapped_channels.get(&channel).copied().unwrap_or(channel)
    }
}

impl Drop for MidiChannelMap {
    fn drop(&mut self) {
        let mut allocator = self.allocator.lock();
        for channel in &self.allocated_channels {
            allocator.release(*channel);
        }
    }
}
