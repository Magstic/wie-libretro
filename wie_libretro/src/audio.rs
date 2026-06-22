use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::{fs, path::PathBuf};

use rustysynth::{SoundFont, Synthesizer, SynthesizerSettings};

pub const AUDIO_SAMPLE_RATE: u32 = 48_000;
pub const AUDIO_FPS: f64 = 60.0;

pub struct AudioState {
    sample_rate: u32,
    pcm: Vec<PcmVoice>,
    left: Vec<f32>,
    right: Vec<f32>,
}

impl AudioState {
    pub fn new(sample_rate: u32) -> Self {
        Self {
            sample_rate,
            pcm: Vec::new(),
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

        for (frame, (left, right)) in output.chunks_exact_mut(2).zip(self.left.iter().zip(&self.right)) {
            frame[0] = float_to_i16(*left);
            frame[1] = float_to_i16(*right);
        }
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

    pub fn note_on(&self, channel: u8, note: u8, velocity: u8) {
        self.write_message(&[0x90 | midi_channel(channel), midi_data(note), midi_data(velocity)]);
    }

    pub fn note_off(&self, channel: u8, note: u8, velocity: u8) {
        self.write_message(&[0x80 | midi_channel(channel), midi_data(note), midi_data(velocity)]);
    }

    pub fn program_change(&self, channel: u8, program: u8) {
        self.write_message(&[0xC0 | midi_channel(channel), midi_data(program)]);
    }

    pub fn control_change(&self, channel: u8, control: u8, value: u8) {
        self.write_message(&[0xB0 | midi_channel(channel), midi_data(control), midi_data(value)]);
    }

    pub fn pitch_bend(&self, channel: u8, value: u16) {
        let value = value.min(16_383);
        self.write_message(&[0xE0 | midi_channel(channel), (value & 0x7f) as u8, ((value >> 7) & 0x7f) as u8]);
    }

    pub fn sysex(&self, data: &[u8]) {
        self.write_message(data);
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
    pcm: Arc<Mutex<AudioState>>,
    midi: Arc<MidiOutput>,
}

impl LibretroAudioSink {
    pub fn new(pcm: Arc<Mutex<AudioState>>, midi: Arc<MidiOutput>) -> Self {
        Self { pcm, midi }
    }
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
            let mut sound_font_data = std::io::Cursor::new(custom_sound_font.as_slice());
            SoundFont::new(&mut sound_font_data).ok()
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

impl wie_backend::AudioSink for LibretroAudioSink {
    fn play_wave(&self, channel: u8, sampling_rate: u32, wave_data: &[i16]) {
        if let Ok(mut pcm) = self.pcm.lock() {
            pcm.push_wave(channel, sampling_rate, wave_data);
        }
    }

    fn midi_note_on(&self, channel_id: u8, note: u8, velocity: u8) {
        self.midi.note_on(channel_id, note, velocity);
    }

    fn midi_note_off(&self, channel_id: u8, note: u8, velocity: u8) {
        self.midi.note_off(channel_id, note, velocity);
    }

    fn midi_program_change(&self, channel_id: u8, program: u8) {
        self.midi.program_change(channel_id, program);
    }

    fn midi_control_change(&self, channel_id: u8, control: u8, value: u8) {
        self.midi.control_change(channel_id, control, value);
    }

    fn midi_pitch_bend(&self, channel_id: u8, value: u16) {
        self.midi.pitch_bend(channel_id, value);
    }

    fn midi_sysex(&self, data: &[u8]) {
        self.midi.sysex(data);
    }
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
    use super::{AUDIO_SAMPLE_RATE, AudioState, MidiOutput};

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
        midi.note_on(0, 60, 100);

        assert!(midi.is_closed());
    }

    #[test]
    fn midi_output_renders_with_embedded_soundfont() {
        let midi = MidiOutput::new(true, None, 5);
        midi.note_on(0, 60, 100);
        let mut rendered = vec![0; 4096];

        midi.render_into(&mut rendered);

        assert!(rendered.iter().any(|sample| *sample != 0));
    }

    #[test]
    fn midi_output_renders_with_custom_soundfont() {
        let path = std::env::temp_dir().join(format!("wie-libretro-{}.sf2", std::process::id()));
        std::fs::write(&path, include_bytes!("../assets/sines.sf2")).unwrap();

        let midi = MidiOutput::new(true, Some(path.clone()), 5);
        midi.note_on(0, 60, 100);
        let mut rendered = vec![0; 4096];
        midi.render_into(&mut rendered);

        let _ = std::fs::remove_file(path);
        assert!(rendered.iter().any(|sample| *sample != 0));
    }

    #[test]
    fn midi_volume_zero_is_silent() {
        let midi = MidiOutput::new(true, None, 0);
        midi.note_on(0, 60, 100);
        let mut rendered = vec![0; 4096];

        midi.render_into(&mut rendered);

        assert!(rendered.iter().all(|sample| *sample == 0));
    }
}
