use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, RecvTimeoutError, Sender},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use rustysynth::{SoundFont, Synthesizer, SynthesizerSettings};
use smaf_renderer::EmbeddedRenderer;
use wie_backend::{AudioCommand, AudioEventData, AudioHandle, AudioSequence, TimedAudioEvent};

pub const AUDIO_SAMPLE_RATE: u32 = 48_000;
pub const AUDIO_FPS: f64 = 60.0;

pub struct AudioState {
    sample_rate: u32,
    pcm: Vec<PcmVoice>,
    smaf: Vec<SmafPcmVoice>,
    left: Vec<f32>,
    right: Vec<f32>,
}

impl AudioState {
    pub fn new(sample_rate: u32) -> Self {
        Self {
            sample_rate,
            pcm: Vec::new(),
            smaf: Vec::new(),
            left: Vec::new(),
            right: Vec::new(),
        }
    }

    pub fn push_wave(&mut self, channels: u8, sample_rate: u32, data: &[i16]) {
        if channels == 0 || sample_rate == 0 || data.is_empty() {
            return;
        }

        self.pcm.push(PcmVoice {
            channels: channels as usize,
            sample_rate,
            data: data.to_vec(),
            position: 0.0,
        });
    }

    pub fn clear(&mut self) {
        self.pcm.clear();
        self.smaf.clear();
        self.left.clear();
        self.right.clear();
    }

    pub fn render(&mut self, output: &mut [i16]) {
        let frames = output.len() / 2;
        self.left.resize(frames, 0.0);
        self.right.resize(frames, 0.0);
        self.left.fill(0.0);
        self.right.fill(0.0);

        let mut index = 0;
        while index < self.pcm.len() {
            let finished = mix_voice(&mut self.pcm[index], self.sample_rate, &mut self.left, &mut self.right);
            if finished {
                self.pcm.swap_remove(index);
            } else {
                index += 1;
            }
        }

        let mut index = 0;
        while index < self.smaf.len() {
            let finished = self.smaf[index].mix(self.sample_rate, &mut self.left, &mut self.right);
            if finished {
                self.smaf.remove(index);
            } else {
                index += 1;
            }
        }

        for (frame, (left, right)) in output.chunks_exact_mut(2).zip(self.left.iter().zip(&self.right)) {
            frame[0] = float_to_i16(*left);
            frame[1] = float_to_i16(*right);
        }
    }

    fn start_smaf(&mut self, handle: AudioHandle, sequence: &AudioSequence, repeat: bool) {
        self.stop(handle);
        for event in &sequence.events {
            if let AudioEventData::Smaf { sampling_rate, events } = &event.data {
                self.smaf
                    .push(SmafPcmVoice::new(handle, *sampling_rate, events.clone(), sequence.duration, repeat));
            }
        }
    }

    fn stop(&mut self, handle: AudioHandle) {
        self.smaf.retain(|voice| voice.handle != handle);
    }
}

pub struct MidiOutput {
    enabled: AtomicBool,
    closed: AtomicBool,
    sound_font_path: Option<PathBuf>,
    volume: u8,
    midi: Mutex<Option<SoftwareMidiSynth>>,
}

impl MidiOutput {
    pub fn new(enabled: bool, sound_font_path: Option<PathBuf>, volume: u8) -> Self {
        Self {
            enabled: AtomicBool::new(enabled),
            closed: AtomicBool::new(false),
            midi: Mutex::new(enabled.then(|| SoftwareMidiSynth::new(sound_font_path.as_ref(), volume)).flatten()),
            sound_font_path,
            volume,
        }
    }

    pub fn set_enabled(&self, enabled: bool) {
        if self.closed.load(Ordering::Acquire) {
            return;
        }

        let was_enabled = self.enabled.swap(enabled, Ordering::AcqRel);
        if was_enabled && !enabled {
            self.silence();
        } else if !was_enabled
            && enabled
            && let Ok(mut midi) = self.midi.lock()
            && midi.is_none()
        {
            *midi = SoftwareMidiSynth::new(self.sound_font_path.as_ref(), self.volume);
        }
    }

    pub fn note_off(&self, channel: u8, note: u8, velocity: u8) {
        self.write_message(&[0x80 | midi_channel(channel), midi_data(note), midi_data(velocity)]);
    }

    pub fn control_change(&self, channel: u8, control: u8, value: u8) {
        self.write_message(&[0xB0 | midi_channel(channel), midi_data(control), midi_data(value)]);
    }

    pub fn silence(&self) {
        if self.closed.load(Ordering::Acquire) {
            return;
        }

        for channel in 0..16 {
            self.send(&[0xB0 | channel, 64, 0]);
            self.send(&[0xB0 | channel, 120, 0]);
            self.send(&[0xB0 | channel, 123, 0]);
            self.send(&[0xE0 | channel, 0, 64]);
        }
    }

    pub fn shutdown(&self) {
        if self.closed.swap(true, Ordering::AcqRel) {
            return;
        }

        // ponytail: close all 16 channels; track-specific cleanup is not worth trusting during DLL unload.
        for channel in 0..16 {
            self.send(&[0xB0 | channel, 64, 0]);
            self.send(&[0xB0 | channel, 120, 0]);
            self.send(&[0xB0 | channel, 123, 0]);
            self.send(&[0xE0 | channel, 0, 64]);
        }

        if let Ok(mut midi) = self.midi.lock() {
            let _ = midi.take();
        }
    }

    pub fn render_into(&self, output: &mut [i16]) {
        if self.enabled.load(Ordering::Acquire)
            && !self.closed.load(Ordering::Acquire)
            && let Ok(mut midi) = self.midi.lock()
            && let Some(midi) = midi.as_mut()
        {
            midi.render_into(output);
        }
    }

    #[cfg(test)]
    fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Acquire)
    }

    fn write_message(&self, bytes: &[u8]) {
        if self.enabled.load(Ordering::Acquire) && !self.closed.load(Ordering::Acquire) {
            self.send(bytes);
        }
    }

    fn send(&self, bytes: &[u8]) {
        let Ok(mut midi) = self.midi.lock() else {
            return;
        };
        if let Some(midi) = midi.as_mut() {
            midi.send(bytes);
        }
    }
}

pub struct LibretroAudioSink {
    tx: AudioCommandSender,
}

impl LibretroAudioSink {
    pub fn new(tx: AudioCommandSender) -> Self {
        Self { tx }
    }
}

impl wie_backend::AudioSink for LibretroAudioSink {
    fn send(&self, command: AudioCommand) {
        self.tx.send(command);
    }
}

enum AudioWorkerCommand {
    Audio(AudioCommand),
    Shutdown,
}

#[derive(Clone)]
pub struct AudioCommandSender(Sender<AudioWorkerCommand>);

impl AudioCommandSender {
    fn send(&self, command: AudioCommand) {
        if self.0.send(AudioWorkerCommand::Audio(command)).is_err() {
            tracing::warn!("Libretro audio worker is unavailable");
        }
    }
}

pub struct AudioWorker {
    tx: Sender<AudioWorkerCommand>,
    worker: Option<JoinHandle<()>>,
}

impl AudioWorker {
    pub fn new(pcm: Arc<Mutex<AudioState>>, midi: Arc<MidiOutput>) -> std::io::Result<(Self, AudioCommandSender)> {
        let (tx, rx) = mpsc::channel();
        let worker = thread::Builder::new()
            .name("wie-libretro-audio".into())
            .spawn(move || run_audio_worker(rx, pcm, midi))?;

        Ok((
            Self {
                tx: tx.clone(),
                worker: Some(worker),
            },
            AudioCommandSender(tx),
        ))
    }

    pub fn shutdown(&mut self) {
        let _ = self.tx.send(AudioWorkerCommand::Shutdown);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

impl Drop for AudioWorker {
    fn drop(&mut self) {
        self.shutdown();
    }
}

struct Playback {
    sequence: Arc<AudioSequence>,
    repeat: bool,
    started_at: Instant,
    next_event: usize,
    active_notes: BTreeSet<(u8, u8)>,
    used_channels: BTreeSet<u8>,
}

impl Playback {
    fn new(sequence: Arc<AudioSequence>, repeat: bool) -> Self {
        Self {
            sequence,
            repeat,
            started_at: Instant::now(),
            next_event: 0,
            active_notes: BTreeSet::new(),
            used_channels: BTreeSet::new(),
        }
    }

    fn next_deadline(&self) -> Instant {
        let time = self
            .sequence
            .events
            .get(self.next_event)
            .map_or(self.sequence.duration, |event| event.time);
        self.started_at + Duration::from_millis(time)
    }
}

fn run_audio_worker(rx: Receiver<AudioWorkerCommand>, pcm: Arc<Mutex<AudioState>>, midi: Arc<MidiOutput>) {
    let mut playbacks = BTreeMap::new();

    loop {
        let command = if let Some(deadline) = playbacks.values().map(Playback::next_deadline).min() {
            match rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
                Ok(command) => Some(command),
                Err(RecvTimeoutError::Timeout) => None,
                Err(RecvTimeoutError::Disconnected) => break,
            }
        } else {
            match rx.recv() {
                Ok(command) => Some(command),
                Err(_) => break,
            }
        };

        if let Some(command) = command {
            match command {
                AudioWorkerCommand::Audio(AudioCommand::Play { handle, sequence, repeat }) => {
                    if let Some(mut playback) = playbacks.remove(&handle) {
                        cleanup_playback(&midi, &mut playback);
                    }
                    if let Ok(mut pcm) = pcm.lock() {
                        pcm.start_smaf(handle, &sequence, repeat);
                    }
                    playbacks.insert(handle, Playback::new(sequence, repeat));
                }
                AudioWorkerCommand::Audio(AudioCommand::Stop { handle }) => {
                    if let Some(mut playback) = playbacks.remove(&handle) {
                        cleanup_playback(&midi, &mut playback);
                    }
                    if let Ok(mut pcm) = pcm.lock() {
                        pcm.stop(handle);
                    }
                }
                AudioWorkerCommand::Shutdown => break,
            }
            continue;
        }

        let now = Instant::now();
        let handles: Vec<AudioHandle> = playbacks.keys().copied().collect();
        for handle in handles {
            let playback = playbacks.get_mut(&handle).unwrap();

            while let Some(event) = playback.sequence.events.get(playback.next_event) {
                if playback.started_at + Duration::from_millis(event.time) > now {
                    break;
                }

                play_audio_event(&pcm, &midi, event, &mut playback.active_notes, &mut playback.used_channels);
                playback.next_event += 1;
            }

            if playback.next_event == playback.sequence.events.len() && playback.started_at + Duration::from_millis(playback.sequence.duration) <= now
            {
                cleanup_playback(&midi, playback);

                if playback.repeat && playback.sequence.duration != 0 {
                    playback.started_at = now;
                    playback.next_event = 0;
                } else {
                    playbacks.remove(&handle);
                }
            }
        }
    }

    for playback in playbacks.values_mut() {
        cleanup_playback(&midi, playback);
    }
    if let Ok(mut pcm) = pcm.lock() {
        pcm.smaf.clear();
    }
}

fn play_audio_event(
    pcm: &Mutex<AudioState>,
    midi: &MidiOutput,
    event: &TimedAudioEvent,
    active_notes: &mut BTreeSet<(u8, u8)>,
    used_channels: &mut BTreeSet<u8>,
) {
    match &event.data {
        AudioEventData::Midi(data) => {
            if let Some(status) = data.first().copied()
                && (0x80..0xf0).contains(&status)
            {
                let channel = status & 0x0f;
                used_channels.insert(channel);

                if let Some(note) = data.get(1).copied() {
                    match status & 0xf0 {
                        0x80 => {
                            active_notes.remove(&(channel, note));
                        }
                        0x90 if data.get(2).copied().unwrap_or(0) == 0 => {
                            active_notes.remove(&(channel, note));
                        }
                        0x90 => {
                            active_notes.insert((channel, note));
                        }
                        _ => {}
                    }
                }
            }

            midi.write_message(data);
        }
        AudioEventData::Wave {
            channels,
            sampling_rate,
            samples,
        } => {
            if let Ok(mut pcm) = pcm.lock() {
                pcm.push_wave(*channels, *sampling_rate, samples);
            }
        }
        AudioEventData::Smaf { .. } => {}
    }
}

fn cleanup_playback(midi: &MidiOutput, playback: &mut Playback) {
    for (channel, note) in &playback.active_notes {
        midi.note_off(*channel, *note, 0);
    }
    for channel in &playback.used_channels {
        midi.control_change(*channel, 64, 0);
        midi.control_change(*channel, 120, 0);
        midi.control_change(*channel, 123, 0);
    }

    playback.active_notes.clear();
    playback.used_channels.clear();
}

struct SoftwareMidiSynth {
    synth: Synthesizer,
    left: Vec<f32>,
    right: Vec<f32>,
    gain: f32,
}

impl SoftwareMidiSynth {
    fn new(sound_font_path: Option<&PathBuf>, volume: u8) -> Option<Self> {
        let custom_sound_font = sound_font_path.and_then(|path| fs::read(path).ok());
        let sound_font = if let Some(custom_sound_font) = custom_sound_font {
            load_sound_font(&custom_sound_font).or_else(|| {
                let mut patched = custom_sound_font;
                patch_sound_font(&mut patched);
                load_sound_font(&patched)
            })
        } else {
            None
        };
        let sound_font = match sound_font {
            Some(sound_font) => Arc::new(sound_font),
            None => {
                let mut sound_font_data = std::io::Cursor::new(include_bytes!("../assets/sines.sf2").as_slice());
                Arc::new(SoundFont::new(&mut sound_font_data).ok()?)
            }
        };
        let settings = SynthesizerSettings::new(AUDIO_SAMPLE_RATE as i32);
        let synth = Synthesizer::new(&sound_font, &settings).ok()?;

        Some(Self {
            synth,
            left: Vec::new(),
            right: Vec::new(),
            gain: volume.min(10) as f32 / 5.0,
        })
    }

    fn send(&mut self, bytes: &[u8]) {
        let Some(status) = bytes.first().copied() else {
            return;
        };
        let command = status & 0xf0;
        if !matches!(command, 0x80 | 0x90 | 0xb0 | 0xc0 | 0xe0) {
            return;
        }

        self.synth.process_midi_message(
            (status & 0x0f) as i32,
            command as i32,
            bytes.get(1).copied().unwrap_or(0) as i32,
            bytes.get(2).copied().unwrap_or(0) as i32,
        );
    }

    fn render_into(&mut self, output: &mut [i16]) {
        let frames = output.len() / 2;
        self.left.resize(frames, 0.0);
        self.right.resize(frames, 0.0);
        self.synth.render(&mut self.left, &mut self.right);

        for (frame, (left, right)) in output.chunks_exact_mut(2).zip(self.left.iter().zip(&self.right)) {
            frame[0] = mix_i16_f32(frame[0], *left * self.gain);
            frame[1] = mix_i16_f32(frame[1], *right * self.gain);
        }
    }
}

fn load_sound_font(data: &[u8]) -> Option<SoundFont> {
    SoundFont::new(&mut std::io::Cursor::new(data)).ok()
}

fn patch_sound_font(data: &mut Vec<u8>) {
    patch_empty_presets(data);
    patch_sample_loops(data);
}

fn patch_empty_presets(data: &mut Vec<u8>) -> Option<()> {
    const PHDR_SIZE: usize = 38;

    let (pdta_offset, pdta_start, pdta_end) = find_list(data, b"pdta")?;
    let (phdr_offset, phdr_start, phdr_size) = find_chunk(data, pdta_start + 4, pdta_end, b"phdr")?;
    if phdr_size % PHDR_SIZE != 0 {
        return None;
    }

    let count = phdr_size / PHDR_SIZE;
    let mut phdr = Vec::with_capacity(phdr_size);
    for index in 0..count {
        let record = phdr_start + index * PHDR_SIZE;
        if index + 1 == count || read_u16(data, record + 24)? < read_u16(data, record + PHDR_SIZE + 24)? {
            phdr.extend_from_slice(&data[record..record + PHDR_SIZE]);
        }
    }

    let delta = phdr_size.checked_sub(phdr.len())?;
    if delta == 0 {
        return Some(());
    }

    data.splice(phdr_start..phdr_start + phdr_size, phdr);
    write_u32(data, phdr_offset + 4, (phdr_size - delta) as u32)?;
    let pdta_size = read_u32(data, pdta_offset + 4)?.checked_sub(delta as u32)?;
    let riff_size = read_u32(data, 4)?.checked_sub(delta as u32)?;
    write_u32(data, pdta_offset + 4, pdta_size)?;
    write_u32(data, 4, riff_size)?;
    Some(())
}

fn patch_sample_loops(data: &mut [u8]) -> Option<()> {
    const SHDR_SIZE: usize = 46;

    let (_, sdta_start, sdta_end) = find_list(data, b"sdta")?;
    let (_, _, smpl_size) = find_chunk(data, sdta_start + 4, sdta_end, b"smpl")?;
    let sample_count = (smpl_size / 2) as u32;

    let (_, pdta_start, pdta_end) = find_list(data, b"pdta")?;
    let (_, shdr_start, shdr_size) = find_chunk(data, pdta_start + 4, pdta_end, b"shdr")?;
    if shdr_size < SHDR_SIZE || shdr_size % SHDR_SIZE != 0 || sample_count == 0 {
        return None;
    }

    for record in (shdr_start..shdr_start + shdr_size - SHDR_SIZE).step_by(SHDR_SIZE) {
        let mut start = read_u32(data, record + 20)?;
        let end = read_u32(data, record + 24)?.min(sample_count);
        let mut start_loop = read_u32(data, record + 28)?;
        let mut end_loop = read_u32(data, record + 32)?;
        if end == 0 {
            continue;
        }

        start = start.min(end - 1);
        if end_loop <= start_loop {
            start_loop = start;
            end_loop = end;
        }
        start_loop = start_loop.clamp(start, end - 1);
        end_loop = end_loop.clamp(start_loop + 1, end);

        write_u32(data, record + 20, start)?;
        write_u32(data, record + 24, end)?;
        write_u32(data, record + 28, start_loop)?;
        write_u32(data, record + 32, end_loop)?;
    }

    Some(())
}

fn find_list(data: &[u8], list_type: &[u8; 4]) -> Option<(usize, usize, usize)> {
    let mut offset = 12;
    while offset + 12 <= data.len() {
        let size = read_u32(data, offset + 4)? as usize;
        let start = offset + 8;
        let end = start.checked_add(size)?;
        if end > data.len() {
            return None;
        }
        if size >= 4 && &data[offset..offset + 4] == b"LIST" && &data[start..start + 4] == list_type {
            return Some((offset, start, end));
        }
        offset = end + (size & 1);
    }

    None
}

fn find_chunk(data: &[u8], start: usize, end: usize, id: &[u8; 4]) -> Option<(usize, usize, usize)> {
    let mut offset = start;
    while offset + 8 <= end {
        let size = read_u32(data, offset + 4)? as usize;
        let payload = offset + 8;
        if payload + size > end {
            return None;
        }
        if &data[offset..offset + 4] == id {
            return Some((offset, payload, size));
        }
        offset = payload + size + (size & 1);
    }

    None
}

fn read_u16(data: &[u8], offset: usize) -> Option<u16> {
    Some(u16::from_le_bytes(data.get(offset..offset + 2)?.try_into().ok()?))
}

fn read_u32(data: &[u8], offset: usize) -> Option<u32> {
    Some(u32::from_le_bytes(data.get(offset..offset + 4)?.try_into().ok()?))
}

fn write_u32(data: &mut [u8], offset: usize, value: u32) -> Option<()> {
    data.get_mut(offset..offset + 4)?.copy_from_slice(&value.to_le_bytes());
    Some(())
}

fn mix_voice(voice: &mut PcmVoice, output_sample_rate: u32, left: &mut [f32], right: &mut [f32]) -> bool {
    let source_frames = voice.data.len() / voice.channels;
    if source_frames == 0 {
        return true;
    }

    let ratio = voice.sample_rate as f64 / output_sample_rate as f64;
    for (left, right) in left.iter_mut().zip(right.iter_mut()) {
        let frame_index = voice.position.floor() as usize;
        if frame_index >= source_frames {
            return true;
        }

        let next_index = (frame_index + 1).min(source_frames - 1);
        let frac = (voice.position - frame_index as f64) as f32;
        let (sample_left, sample_right) = voice.sample(frame_index, next_index, frac);
        *left += sample_left;
        *right += sample_right;
        voice.position += ratio;
    }

    voice.position.floor() as usize >= source_frames
}

struct PcmVoice {
    channels: usize,
    sample_rate: u32,
    data: Vec<i16>,
    position: f64,
}

struct SmafPcmVoice {
    handle: AudioHandle,
    renderer: EmbeddedRenderer,
    sample_rate: u32,
    repeat: bool,
    cycle_frames: usize,
    cycle_position: usize,
    current: Option<[i16; 2]>,
    next: Option<[i16; 2]>,
    fraction: f64,
}

impl SmafPcmVoice {
    fn new(handle: AudioHandle, sample_rate: u32, events: Arc<Vec<(usize, smaf_player::SmafEvent)>>, duration_ms: u64, repeat: bool) -> Self {
        let sample_rate = sample_rate.max(8_000);
        let mut result = Self {
            handle,
            renderer: EmbeddedRenderer::new(events, sample_rate),
            sample_rate,
            repeat: repeat && duration_ms != 0,
            cycle_frames: (duration_ms.saturating_mul(u64::from(sample_rate)) / 1000).min(usize::MAX as u64) as usize,
            cycle_position: 0,
            current: None,
            next: None,
            fraction: 0.0,
        };
        result.current = result.pull_source_frame();
        result.next = result.pull_source_frame();
        result
    }

    fn mix(&mut self, output_sample_rate: u32, left: &mut [f32], right: &mut [f32]) -> bool {
        let ratio = self.sample_rate as f64 / output_sample_rate as f64;
        for (left, right) in left.iter_mut().zip(right.iter_mut()) {
            let Some(current) = self.current else {
                return true;
            };
            let next = self.next.unwrap_or(current);
            let fraction = self.fraction as f32;
            *left += lerp_i16(current[0], next[0], fraction) / i16::MAX as f32;
            *right += lerp_i16(current[1], next[1], fraction) / i16::MAX as f32;

            self.fraction += ratio;
            while self.fraction >= 1.0 {
                self.current = self.next;
                self.next = self.pull_source_frame();
                self.fraction -= 1.0;
                if self.current.is_none() {
                    return true;
                }
            }
        }

        self.current.is_none()
    }

    fn pull_source_frame(&mut self) -> Option<[i16; 2]> {
        if self.repeat && self.cycle_position >= self.cycle_frames {
            self.renderer.restart_cycle();
            self.cycle_position = 0;
        }

        if let Some(frame) = self.renderer.next_frame() {
            self.cycle_position = self.cycle_position.saturating_add(1);
            return Some(frame);
        }

        if self.repeat && self.cycle_position < self.cycle_frames {
            self.cycle_position += 1;
            Some([0; 2])
        } else {
            None
        }
    }
}

impl PcmVoice {
    fn sample(&self, frame_index: usize, next_index: usize, frac: f32) -> (f32, f32) {
        let left = self.sample_channel(frame_index, 0);
        let right = if self.channels == 1 { left } else { self.sample_channel(frame_index, 1) };
        let next_left = self.sample_channel(next_index, 0);
        let next_right = if self.channels == 1 {
            next_left
        } else {
            self.sample_channel(next_index, 1)
        };

        (
            lerp_i16(left, next_left, frac) / i16::MAX as f32,
            lerp_i16(right, next_right, frac) / i16::MAX as f32,
        )
    }

    fn sample_channel(&self, frame_index: usize, channel_index: usize) -> i16 {
        let channel_index = channel_index.min(self.channels - 1);
        self.data[frame_index * self.channels + channel_index]
    }
}

fn lerp_i16(a: i16, b: i16, frac: f32) -> f32 {
    a as f32 + (b as f32 - a as f32) * frac
}

fn midi_channel(channel: u8) -> u8 {
    channel & 0x0f
}

fn midi_data(value: u8) -> u8 {
    value & 0x7f
}

fn mix_i16_f32(sample: i16, add: f32) -> i16 {
    float_to_i16(sample as f32 / i16::MAX as f32 + add)
}

fn float_to_i16(sample: f32) -> i16 {
    let sample = sample.clamp(-1.0, 1.0);
    if sample >= 0.0 {
        (sample * i16::MAX as f32) as i16
    } else {
        (sample * -(i16::MIN as f32)) as i16
    }
}

#[cfg(test)]
mod tests {
    use std::{sync::Arc, time::Instant};

    use smaf_player::{SmafEvent, WaveDynamics};
    use wie_backend::{AudioCommand, AudioEventData, AudioSequence, TimedAudioEvent};

    use super::{AUDIO_SAMPLE_RATE, AudioState, AudioWorker, MidiOutput};

    #[test]
    fn audio_worker_dispatches_timed_wave_events() {
        let audio = Arc::new(std::sync::Mutex::new(AudioState::new(AUDIO_SAMPLE_RATE)));
        let midi = Arc::new(MidiOutput::new(false, None, 5));
        let (mut worker, tx) = AudioWorker::new(audio.clone(), midi).unwrap();
        tx.send(AudioCommand::Play {
            handle: 1,
            sequence: Arc::new(AudioSequence {
                duration: 0,
                events: vec![TimedAudioEvent {
                    time: 0,
                    data: AudioEventData::Wave {
                        channels: 1,
                        sampling_rate: AUDIO_SAMPLE_RATE,
                        samples: vec![i16::MAX],
                    },
                }],
            }),
            repeat: false,
        });

        let timeout = Instant::now() + std::time::Duration::from_secs(1);
        while audio.lock().unwrap().pcm.is_empty() && Instant::now() < timeout {
            std::thread::yield_now();
        }

        worker.shutdown();
        assert_eq!(audio.lock().unwrap().pcm.len(), 1);
    }

    #[test]
    fn wave_renders_stereo_frames() {
        let mut audio = AudioState::new(AUDIO_SAMPLE_RATE);
        audio.push_wave(1, AUDIO_SAMPLE_RATE, &[i16::MAX, 0]);
        let mut rendered = vec![0; 4];

        audio.render(&mut rendered);

        assert!(rendered[0] > 0);
        assert!(rendered[1] > 0);
    }

    #[test]
    fn streamed_smaf_matches_the_previous_eager_stem_after_resampling() {
        let events = Arc::new(vec![(
            3,
            SmafEvent::Wave {
                channels: 1,
                sampling_rate: 8_000,
                data: vec![0, 4_000, 16_000, -8_000, 0],
                dynamics: WaveDynamics {
                    velocity: 101,
                    volume: 112,
                    expression: 93,
                    pan: Some(79),
                },
            },
        )]);
        let source_rate = 24_000;
        let eager = smaf_renderer::render_embedded_audio(&events, source_rate);
        let sequence = AudioSequence {
            duration: 20,
            events: vec![TimedAudioEvent {
                time: 0,
                data: AudioEventData::Smaf {
                    sampling_rate: source_rate,
                    events,
                },
            }],
        };
        let mut previous = AudioState::new(AUDIO_SAMPLE_RATE);
        previous.push_wave(eager.channels, eager.sampling_rate, &eager.data);
        let mut streamed = AudioState::new(AUDIO_SAMPLE_RATE);
        streamed.start_smaf(7, &sequence, false);
        let mut previous_output = vec![0; 256];
        let mut streamed_output = vec![0; 256];

        previous.render(&mut previous_output);
        streamed.render(&mut streamed_output);

        assert_eq!(streamed_output, previous_output);
    }

    #[test]
    fn stopping_a_smaf_handle_removes_its_streaming_voice() {
        let sequence = AudioSequence {
            duration: 100,
            events: vec![TimedAudioEvent {
                time: 0,
                data: AudioEventData::Smaf {
                    sampling_rate: 24_000,
                    events: Arc::new(vec![(
                        0,
                        SmafEvent::Wave {
                            channels: 1,
                            sampling_rate: 8_000,
                            data: vec![i16::MAX; 800],
                            dynamics: WaveDynamics::UNITY,
                        },
                    )]),
                },
            }],
        };
        let mut audio = AudioState::new(AUDIO_SAMPLE_RATE);
        audio.start_smaf(3, &sequence, false);
        audio.stop(3);
        let mut output = vec![1; 32];

        audio.render(&mut output);

        assert_eq!(output, vec![0; 32]);
    }

    #[test]
    fn shutdown_clears_audio_and_closes_midi_path() {
        let mut audio = AudioState::new(AUDIO_SAMPLE_RATE);
        audio.push_wave(1, AUDIO_SAMPLE_RATE, &[i16::MAX, 0]);
        audio.clear();
        let mut rendered = vec![1; 4];

        audio.render(&mut rendered);

        assert_eq!(rendered, vec![0; 4]);

        let midi = MidiOutput::new(false, None, 5);
        midi.shutdown();
        midi.set_enabled(true);
        midi.write_message(&[0x90, 60, 100]);

        assert!(midi.is_closed());
    }

    #[test]
    fn midi_output_renders_with_embedded_soundfont() {
        let midi = MidiOutput::new(true, None, 5);
        midi.write_message(&[0x90, 60, 100]);
        let mut rendered = vec![0; 4096];

        midi.render_into(&mut rendered);

        assert!(rendered.iter().any(|sample| *sample != 0));
    }

    #[test]
    fn midi_output_renders_with_custom_soundfont() {
        let path = std::env::temp_dir().join(format!("wie-libretro-{}.sf2", std::process::id()));
        std::fs::write(&path, include_bytes!("../assets/sines.sf2")).unwrap();

        let midi = MidiOutput::new(true, Some(path.clone()), 5);
        midi.write_message(&[0x90, 60, 100]);
        let mut rendered = vec![0; 4096];
        midi.render_into(&mut rendered);

        let _ = std::fs::remove_file(path);
        assert!(rendered.iter().any(|sample| *sample != 0));
    }

    #[test]
    fn midi_output_loads_lenient_soundfonts_if_present() {
        for path in [
            "C:\\Users\\Magstic\\Documents\\SynthFont\\Nokia_Lloyd_Bank2.sf2",
            "C:\\Users\\Magstic\\Documents\\SynthFont\\MCP-MA7.sf2",
        ] {
            let path = std::path::PathBuf::from(path);
            if !path.exists() {
                continue;
            }

            let midi = MidiOutput::new(true, Some(path), 5);
            midi.write_message(&[0x90, 60, 100]);
            let mut rendered = vec![0; 4096];
            midi.render_into(&mut rendered);

            assert!(rendered.iter().any(|sample| *sample != 0));
        }
    }

    #[test]
    fn midi_volume_zero_is_silent() {
        let midi = MidiOutput::new(true, None, 0);
        midi.write_message(&[0x90, 60, 100]);
        let mut rendered = vec![0; 4096];

        midi.render_into(&mut rendered);

        assert!(rendered.iter().all(|sample| *sample == 0));
    }
}
