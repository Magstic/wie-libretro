use alloc::{
    boxed::Box,
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
    vec::Vec,
};
use core::sync::atomic::{AtomicBool, Ordering};

#[cfg(not(target_arch = "wasm32"))]
extern crate std;

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
    #[cfg(not(target_arch = "wasm32"))]
    scheduler: AudioScheduler,
    files: BTreeMap<AudioHandle, AudioFile>,
    playing: BTreeMap<AudioHandle, Arc<PlaybackControl>>,
    last_audio_handle: AudioHandle,
}

impl Audio {
    pub fn new(sink: Box<dyn AudioSink>) -> Self {
        Self {
            sink: Arc::new(sink),
            midi_channels: Arc::new(Mutex::new(MidiChannelAllocator::default())),
            #[cfg(not(target_arch = "wasm32"))]
            scheduler: AudioScheduler::new(),
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

        let sink_clone = self.sink.clone();
        let midi_channel_map = MidiChannelMap::allocate(self.midi_channels.clone(), &player.used_midi_channels());

        let playback_control = Arc::new(PlaybackControl::new());
        self.playing.insert(audio_handle, playback_control.clone());

        let loop_count = normalize_loop_count(loop_count);

        #[cfg(not(target_arch = "wasm32"))]
        {
            let _ = system;
            let playback = NativePlayback::new(player, sink_clone, midi_channel_map, playback_control, loop_count);
            self.scheduler.play(playback).map_err(|_| AudioError::InvalidAudio)?;
        }

        #[cfg(target_arch = "wasm32")]
        {
            let mut system_clone = system.clone();
            system.spawn(async move || {
                let mut remaining = loop_count;

                loop {
                    player
                        .play_once(&mut system_clone, &**sink_clone, &playback_control, &midi_channel_map)
                        .await;

                    if playback_control.is_stopped() || !player.can_repeat() {
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
        }

        Ok(())
    }

    pub fn stop(&mut self, audio_handle: AudioHandle) {
        if let Some(playback_control) = self.playing.remove(&audio_handle) {
            playback_control.stop();
            #[cfg(not(target_arch = "wasm32"))]
            self.scheduler.wake();
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

struct PlaybackControl {
    stopped: AtomicBool,
}

impl PlaybackControl {
    fn new() -> Self {
        Self {
            stopped: AtomicBool::new(false),
        }
    }

    fn is_stopped(&self) -> bool {
        self.stopped.load(Ordering::Relaxed)
    }

    fn stop(&self) {
        self.stopped.store(true, Ordering::Relaxed);
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

    #[cfg(target_arch = "wasm32")]
    async fn play_once(&self, system: &mut System, sink: &dyn AudioSink, playback_control: &PlaybackControl, midi_channel_map: &MidiChannelMap) {
        let mut active_notes: Vec<(u8, u8)> = Vec::new();
        let mut used_channels: BTreeSet<u8> = BTreeSet::new();

        let start_time = system.platform().now();
        for (time, event) in &self.events {
            if playback_control.is_stopped() {
                break;
            }

            let now = system.platform().now();
            if (*time as u64) > now - start_time {
                system.sleep(((*time as u64) - (now - start_time)) as _).await;

                if playback_control.is_stopped() {
                    break;
                }
            }

            Self::dispatch_event(event, sink, midi_channel_map, &mut active_notes, &mut used_channels);
        }

        Self::finish_playback(sink, &active_notes, &used_channels);
    }

    fn dispatch_event(
        event: &SmafEvent,
        sink: &dyn AudioSink,
        midi_channel_map: &MidiChannelMap,
        active_notes: &mut Vec<(u8, u8)>,
        used_channels: &mut BTreeSet<u8>,
    ) {
        match event {
            SmafEvent::Wave {
                channel,
                sampling_rate,
                data,
            } => sink.play_wave(*channel, *sampling_rate, data),
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
            SmafEvent::MidiSysEx(data) => sink.midi_sysex(data),
            SmafEvent::End => {}
        }
    }

    fn finish_playback(sink: &dyn AudioSink, active_notes: &[(u8, u8)], used_channels: &BTreeSet<u8>) {
        for (channel, note) in active_notes {
            sink.midi_note_off(*channel, *note, 0);
        }

        // Release sustain and force any lingering voices off on every channel
        // this track touched. Tracks that set sustain pedal (CC 64) or use
        // long release envelopes (e.g. drum voices) otherwise keep ringing
        // after note_off or an interrupted playback.
        for channel in used_channels {
            sink.midi_control_change(*channel, 64, 0); // sustain off
            sink.midi_control_change(*channel, 120, 0); // all sound off
            sink.midi_control_change(*channel, 123, 0); // all notes off
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
struct AudioScheduler {
    sender: std::sync::mpsc::Sender<AudioSchedulerCommand>,
}

#[cfg(not(target_arch = "wasm32"))]
enum AudioSchedulerCommand {
    Play(NativePlayback),
    Wake,
}

#[cfg(not(target_arch = "wasm32"))]
impl AudioScheduler {
    fn new() -> Self {
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::Builder::new()
            .name("wie-audio-scheduler".into())
            .spawn(move || {
                use std::sync::mpsc::RecvTimeoutError;
                use std::time::{Duration, Instant};

                let mut playbacks: Vec<NativePlayback> = Vec::new();
                loop {
                    let now = Instant::now();
                    let mut index = 0;
                    while index < playbacks.len() {
                        if playbacks[index].advance(now) {
                            playbacks.remove(index);
                        } else {
                            index += 1;
                        }
                    }

                    let timeout = playbacks
                        .iter()
                        .map(|playback| playback.next_due().saturating_duration_since(now))
                        .min()
                        .unwrap_or(Duration::from_secs(60));
                    match receiver.recv_timeout(timeout) {
                        Ok(AudioSchedulerCommand::Play(playback)) => playbacks.push(playback),
                        Ok(AudioSchedulerCommand::Wake) | Err(RecvTimeoutError::Timeout) => {}
                        Err(RecvTimeoutError::Disconnected) => {
                            for playback in &mut playbacks {
                                playback.finish();
                            }
                            return;
                        }
                    }
                }
            })
            .expect("failed to start audio scheduler thread");

        Self { sender }
    }

    fn play(&self, playback: NativePlayback) -> core::result::Result<(), ()> {
        self.sender.send(AudioSchedulerCommand::Play(playback)).map_err(|_| ())
    }

    fn wake(&self) {
        let _ = self.sender.send(AudioSchedulerCommand::Wake);
    }
}

#[cfg(not(target_arch = "wasm32"))]
struct NativePlayback {
    player: SmafPlayer,
    sink: Arc<Box<dyn AudioSink>>,
    midi_channel_map: MidiChannelMap,
    control: Arc<PlaybackControl>,
    remaining: Option<u32>,
    event_index: usize,
    start_time: std::time::Instant,
    active_notes: Vec<(u8, u8)>,
    used_channels: BTreeSet<u8>,
}

#[cfg(not(target_arch = "wasm32"))]
impl NativePlayback {
    fn new(
        player: SmafPlayer,
        sink: Arc<Box<dyn AudioSink>>,
        midi_channel_map: MidiChannelMap,
        control: Arc<PlaybackControl>,
        remaining: Option<u32>,
    ) -> Self {
        Self {
            player,
            sink,
            midi_channel_map,
            control,
            remaining,
            event_index: 0,
            start_time: std::time::Instant::now(),
            active_notes: Vec::new(),
            used_channels: BTreeSet::new(),
        }
    }

    fn next_due(&self) -> std::time::Instant {
        let Some((time, _)) = self.player.events.get(self.event_index) else {
            return std::time::Instant::now();
        };
        self.start_time
            .checked_add(std::time::Duration::from_millis(*time as u64))
            .unwrap_or(self.start_time)
    }

    fn advance(&mut self, now: std::time::Instant) -> bool {
        loop {
            if self.control.is_stopped() {
                self.finish();
                return true;
            }

            if self.event_index == self.player.events.len() {
                self.finish();
                if !self.player.can_repeat() {
                    return true;
                }
                if let Some(count) = self.remaining.as_mut() {
                    *count = count.saturating_sub(1);
                    if *count == 0 {
                        return true;
                    }
                }

                self.event_index = 0;
                self.start_time = now;
                continue;
            }

            if now < self.next_due() {
                return false;
            }

            let (_, event) = &self.player.events[self.event_index];
            SmafPlayer::dispatch_event(
                event,
                &**self.sink,
                &self.midi_channel_map,
                &mut self.active_notes,
                &mut self.used_channels,
            );
            self.event_index += 1;
        }
    }

    fn finish(&mut self) {
        SmafPlayer::finish_playback(&**self.sink, &self.active_notes, &self.used_channels);
        self.active_notes.clear();
        self.used_channels.clear();
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

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use alloc::{boxed::Box, sync::Arc, vec, vec::Vec};
    use std::{sync::Mutex as StdMutex, time::Instant};

    use super::*;

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum RecordedEvent {
        NoteOn(u8, u8),
        NoteOff(u8, u8),
    }

    struct RecordingSink {
        events: Arc<StdMutex<Vec<(Instant, RecordedEvent)>>>,
    }

    impl AudioSink for RecordingSink {
        fn play_wave(&self, _channel: u8, _sampling_rate: u32, _wave_data: &[i16]) {}

        fn midi_note_on(&self, channel_id: u8, note: u8, _velocity: u8) {
            self.events
                .lock()
                .unwrap()
                .push((Instant::now(), RecordedEvent::NoteOn(channel_id, note)));
        }

        fn midi_note_off(&self, channel_id: u8, note: u8, _velocity: u8) {
            self.events
                .lock()
                .unwrap()
                .push((Instant::now(), RecordedEvent::NoteOff(channel_id, note)));
        }

        fn midi_control_change(&self, _channel_id: u8, _control: u8, _value: u8) {}
        fn midi_program_change(&self, _channel_id: u8, _program: u8) {}
        fn midi_pitch_bend(&self, _channel_id: u8, _value: u16) {}
        fn midi_sysex(&self, _data: &[u8]) {}
    }

    fn playback(events: Vec<(usize, SmafEvent)>, sink: Arc<Box<dyn AudioSink>>, control: Arc<PlaybackControl>) -> NativePlayback {
        let source_channels = BTreeSet::from([0]);
        let channel_map = MidiChannelMap::allocate(Arc::new(Mutex::new(MidiChannelAllocator::default())), &source_channels);
        NativePlayback::new(SmafPlayer { events }, sink, channel_map, control, Some(1))
    }

    #[test]
    fn native_scheduler_dispatches_note_off_without_system_ticks() {
        let recorded = Arc::new(StdMutex::new(Vec::new()));
        let sink: Arc<Box<dyn AudioSink>> = Arc::new(Box::new(RecordingSink { events: recorded.clone() }));
        let control = Arc::new(PlaybackControl::new());
        let scheduler = AudioScheduler::new();
        scheduler
            .play(playback(
                vec![
                    (
                        0,
                        SmafEvent::MidiNoteOn {
                            channel: 0,
                            note: 60,
                            velocity: 100,
                        },
                    ),
                    (
                        30,
                        SmafEvent::MidiNoteOff {
                            channel: 0,
                            note: 60,
                            velocity: 0,
                        },
                    ),
                    (30, SmafEvent::End),
                ],
                sink,
                control,
            ))
            .unwrap();

        let timeout = Instant::now() + std::time::Duration::from_secs(1);
        while recorded.lock().unwrap().len() < 2 && Instant::now() < timeout {
            std::thread::sleep(std::time::Duration::from_millis(2));
        }

        let events = recorded.lock().unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].1, RecordedEvent::NoteOn(0, 60));
        assert_eq!(events[1].1, RecordedEvent::NoteOff(0, 60));
        assert!(events[1].0.duration_since(events[0].0) >= std::time::Duration::from_millis(20));
    }

    #[test]
    fn stopping_playback_wakes_scheduler_and_releases_active_note() {
        let recorded = Arc::new(StdMutex::new(Vec::new()));
        let sink: Arc<Box<dyn AudioSink>> = Arc::new(Box::new(RecordingSink { events: recorded.clone() }));
        let control = Arc::new(PlaybackControl::new());
        let scheduler = AudioScheduler::new();
        scheduler
            .play(playback(
                vec![
                    (
                        0,
                        SmafEvent::MidiNoteOn {
                            channel: 0,
                            note: 64,
                            velocity: 100,
                        },
                    ),
                    (
                        10_000,
                        SmafEvent::MidiNoteOff {
                            channel: 0,
                            note: 64,
                            velocity: 0,
                        },
                    ),
                ],
                sink,
                control.clone(),
            ))
            .unwrap();

        let note_on_timeout = Instant::now() + std::time::Duration::from_secs(1);
        while recorded.lock().unwrap().is_empty() && Instant::now() < note_on_timeout {
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        assert_eq!(recorded.lock().unwrap().first().map(|event| event.1), Some(RecordedEvent::NoteOn(0, 64)));

        let stopped_at = Instant::now();
        control.stop();
        scheduler.wake();
        let note_off_timeout = stopped_at + std::time::Duration::from_millis(500);
        while recorded.lock().unwrap().len() < 2 && Instant::now() < note_off_timeout {
            std::thread::sleep(std::time::Duration::from_millis(2));
        }

        let events = recorded.lock().unwrap();
        assert_eq!(events.last().map(|event| event.1), Some(RecordedEvent::NoteOff(0, 64)));
        assert!(events.last().unwrap().0.duration_since(stopped_at) < std::time::Duration::from_millis(500));
    }
}
