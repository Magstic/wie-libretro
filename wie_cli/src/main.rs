extern crate alloc;

mod audio_sink;
mod config;
mod database;
mod filesystem;
mod gamepad;
mod window;

use core::str;
use std::{
    collections::HashMap,
    error::Error,
    fs::{self, File},
    io::{LineWriter, Write, stderr},
    num::NonZero,
    path::PathBuf,
    sync::{
        Mutex,
        mpsc::{Receiver, Sender, channel},
    },
    thread,
    time::{SystemTime, UNIX_EPOCH},
};

use clap::Parser;
use gilrs::GamepadId;
use midir::{MidiOutput, MidiOutputPort};
use rodio::{DeviceSinkBuilder, buffer::SamplesBuffer, conversions::SampleTypeConverter};
use winit::keyboard::{KeyCode as WinitKeyCode, PhysicalKey};

use wie_backend::{Emulator, Event, Filesystem, Instant, KeyCode, Options, Platform, ProfileSample, Screen, extract_zip};
use wie_j2me::J2MEEmulator;
use wie_ktf::KtfEmulator;
use wie_lgt::LgtEmulator;
use wie_skt::SktEmulator;

use self::{
    audio_sink::AudioSink,
    config::{Config, GamepadInput},
    database::DatabaseRepository,
    filesystem::CliFilesystem,
    gamepad::{GamepadCallbackEvent, GamepadState},
    window::{WindowCallbackEvent, WindowHandle, WindowImpl},
};

struct WieCliPlatform {
    audio_thread_tx: Sender<(u8, u32, Vec<i16>)>,
    database_repository: DatabaseRepository,
    filesystem: CliFilesystem,
    vibrate_tx: Sender<(u64, u8)>,
    window: WindowHandle,
}

impl WieCliPlatform {
    fn new(window: WindowHandle, vibrate_tx: Sender<(u64, u8)>) -> Self {
        let (tx, rx) = channel();
        thread::spawn(|| Self::audio_thread(rx));

        Self {
            audio_thread_tx: tx,
            database_repository: DatabaseRepository::new(),
            filesystem: CliFilesystem::new(),
            vibrate_tx,
            window,
        }
    }

    fn audio_thread(rx: Receiver<(u8, u32, Vec<i16>)>) {
        let default_output = DeviceSinkBuilder::open_default_sink();
        if default_output.is_err() {
            // do nothing if we can't open output
            loop {
                rx.recv().unwrap();
            }
        }

        let output_sink = default_output.unwrap();
        let mixer = output_sink.mixer().clone();

        loop {
            let result = rx.recv();
            if result.is_err() {
                break;
            }
            let (channel, sampling_rate, wave_data) = result.unwrap();

            let Some(channel_count) = NonZero::new(channel.into()) else {
                continue;
            };
            let Some(sample_rate) = NonZero::new(sampling_rate) else {
                continue;
            };

            let buffer = SamplesBuffer::new(
                channel_count,
                sample_rate,
                SampleTypeConverter::new(wave_data.into_iter()).collect::<Vec<_>>(),
            );

            mixer.add(buffer);
        }
    }
}

impl Platform for WieCliPlatform {
    fn screen(&self) -> &dyn Screen {
        &self.window
    }

    fn now(&self) -> Instant {
        let now = SystemTime::now();
        let since_the_epoch = now.duration_since(UNIX_EPOCH).unwrap();

        Instant::from_epoch_millis(since_the_epoch.as_millis() as _)
    }

    fn database_repository(&self) -> &dyn wie_backend::DatabaseRepository {
        &self.database_repository
    }

    fn filesystem(&self) -> &dyn Filesystem {
        &self.filesystem
    }

    fn audio_sink(&self) -> Box<dyn wie_backend::AudioSink> {
        let midi_out = (|| {
            let midi_out = MidiOutput::new("wie_cli")?;
            let midi_ports = midi_out.ports();
            let out_port_index = select_midi_output_port(&midi_out, &midi_ports).ok_or_else(|| anyhow::anyhow!("No MIDI output port"))?;
            let out_port = &midi_ports[out_port_index];

            if let Ok(port_name) = midi_out.port_name(out_port) {
                tracing::info!("Using MIDI output: {port_name}");
            }

            Ok::<_, Box<dyn Error>>(midi_out.connect(out_port, "wie_cli")?)
        })()
        .ok();

        Box::new(AudioSink::new(midi_out, self.audio_thread_tx.clone()))
    }

    fn write_stdout(&self, buf: &[u8]) {
        let str = str::from_utf8(buf).unwrap();

        print!("{str}")
    }

    fn write_stderr(&self, buf: &[u8]) {
        let str = str::from_utf8(buf).unwrap();

        eprint!("{str}")
    }

    fn exit(&self) {
        self.window.send_quit_event();
    }

    fn vibrate(&self, duration_ms: u64, intensity: u8) {
        if let Err(err) = self.vibrate_tx.send((duration_ms, intensity)) {
            tracing::debug!("Failed to queue gamepad vibration: {err}");
        }
    }
}

fn select_midi_output_port(midi_out: &MidiOutput, midi_ports: &[MidiOutputPort]) -> Option<usize> {
    select_midi_output_port_by_name(midi_ports.iter().map(|port| midi_out.port_name(port).unwrap_or_default()))
}

fn select_midi_output_port_by_name<I, S>(port_names: I) -> Option<usize>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut has_port = false;
    for (index, port_name) in port_names.into_iter().enumerate() {
        has_port = true;
        if is_virtual_midi_synth_port(port_name.as_ref()) {
            return Some(index);
        }
    }

    has_port.then_some(0)
}

fn is_virtual_midi_synth_port(port_name: &str) -> bool {
    port_name.to_ascii_lowercase().contains("virtualmidisynth")
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum InputSource {
    Keyboard(WinitKeyCode),
    Gamepad { id: GamepadId, input: GamepadInput },
}

#[derive(Default)]
struct InputState {
    repeat_times: HashMap<KeyCode, SystemTime>,
    sources: HashMap<InputSource, KeyCode>,
    pressed_counts: HashMap<KeyCode, usize>,
}

impl InputState {
    fn press(&mut self, source: InputSource, keycode: KeyCode) -> bool {
        if self.sources.contains_key(&source) {
            return false;
        }

        self.sources.insert(source, keycode);

        let count = self.pressed_counts.entry(keycode).or_default();
        let send_keydown = *count == 0;
        *count += 1;

        if send_keydown {
            self.repeat_times.insert(keycode, SystemTime::now());
        }

        send_keydown
    }

    fn release(&mut self, source: InputSource) -> Option<KeyCode> {
        let keycode = self.sources.remove(&source)?;
        let count = self.pressed_counts.get_mut(&keycode)?;

        if *count > 1 {
            *count -= 1;
            return None;
        }

        self.pressed_counts.remove(&keycode);
        self.repeat_times.remove(&keycode);

        Some(keycode)
    }

    fn repeat_due(&mut self, now: SystemTime) -> Vec<KeyCode> {
        let mut repeated = Vec::new();

        for (keycode, last_repeat) in &mut self.repeat_times {
            let should_repeat = now
                .duration_since(*last_repeat)
                .map(|duration| duration.as_millis() > 100)
                .unwrap_or(false);

            if should_repeat {
                repeated.push(*keycode);
                *last_repeat = now;
            }
        }

        repeated
    }
}

#[derive(Parser)]
struct Args {
    filename: String,
    #[arg(long, default_value_t = false)]
    debug: bool,
    /// Write a flamegraph-folded sampling profile to this path (one line per
    /// flushed batch; `flamegraph.pl` aggregates duplicates).
    #[arg(long)]
    profile_out: Option<PathBuf>,
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_writer(stderr)
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let args = Args::parse();

    let profile = args.profile_out.as_ref().map(|path| profile_callback(path)).transpose()?;
    let options = Options {
        enable_gdbserver: args.debug,
        profile,
    };

    start(&args.filename, options)
}

fn profile_callback(path: &PathBuf) -> anyhow::Result<wie_backend::ProfileCallback> {
    let writer = Mutex::new(LineWriter::new(File::create(path)?));
    Ok(Box::new(move |batch: Vec<ProfileSample>| {
        let mut writer = writer.lock().unwrap();
        for sample in batch {
            let folded: Vec<String> = sample.stack.iter().rev().map(|pc| format!("0x{pc:x}")).collect();
            let _ = writeln!(writer, "{} {}", folded.join(";"), sample.count);
        }
    }))
}

pub fn start(filename: &str, options: Options) -> anyhow::Result<()> {
    let config_path = config_path()?;
    let config = Config::load(&config_path)?;
    let keyboard_map = config.keyboard_map().clone();
    let gamepad_map = config.gamepad_map().clone();
    let (vibrate_tx, vibrate_rx) = channel();
    let mut gamepad = match GamepadState::new(vibrate_rx) {
        Ok(gamepad) => Some(gamepad),
        Err(err) => {
            tracing::warn!("Failed to initialize gamepad support: {err}");
            None
        }
    };
    let window = WindowImpl::new(240, 320).unwrap(); // TODO hardcoded size
    let platform = Box::new(WieCliPlatform::new(window.handle(), vibrate_tx));

    let buf = fs::read(filename)?;
    let mut emulator: Box<dyn Emulator> = if filename.ends_with("zip") {
        let files = extract_zip(&buf).unwrap();

        if KtfEmulator::loadable_archive(&files) {
            Box::new(KtfEmulator::from_archive(platform, files, options)?)
        } else if LgtEmulator::loadable_archive(&files) {
            Box::new(LgtEmulator::from_archive(platform, files, options)?)
        } else if SktEmulator::loadable_archive(&files) {
            Box::new(SktEmulator::from_archive(platform, files)?)
        } else {
            anyhow::bail!("Unknown archive format");
        }
    } else if filename.ends_with("jad") {
        let jar_filename = filename.replace(".jad", ".jar");
        let jar = fs::read(&jar_filename)?;

        let jar_filename = jar_filename[jar_filename.rfind('/').unwrap_or(0) + 1..].to_owned();

        Box::new(J2MEEmulator::from_jad_jar(platform, buf, jar_filename, jar)?)
    } else if filename.ends_with("jar") {
        let filename_without_path = filename[filename.rfind('/').unwrap_or(0) + 1..].to_owned();
        let filename_without_ext = filename_without_path.trim_end_matches(".jar");

        if KtfEmulator::loadable_jar(&buf) {
            Box::new(KtfEmulator::from_jar(
                platform,
                &filename_without_path,
                buf,
                filename_without_ext,
                filename_without_ext,
                None,
                options,
            )?)
        } else if LgtEmulator::loadable_jar(&buf) {
            Box::new(LgtEmulator::from_jar(
                platform,
                &filename_without_path,
                buf,
                filename_without_ext,
                filename_without_ext,
                None,
                options,
            )?)
        } else if SktEmulator::loadable_jar(&buf) {
            Box::new(SktEmulator::from_jar(platform, &filename_without_path, buf, filename_without_ext, None)?)
        } else {
            Box::new(J2MEEmulator::from_jar(platform, &filename_without_path, buf)?)
        }
    } else {
        anyhow::bail!("Unknown file format");
    };

    let mut input_state = InputState::default();
    window.run(move |event| {
        match event {
            WindowCallbackEvent::Update => {
                let now = SystemTime::now();

                if let Some(gamepad) = gamepad.as_mut() {
                    for gamepad_event in gamepad.poll() {
                        handle_gamepad_event(&mut *emulator, &gamepad_map, &mut input_state, gamepad_event);
                    }
                }

                for keycode in input_state.repeat_due(now) {
                    emulator.handle_event(Event::Keyrepeat(keycode));
                }

                emulator.tick()?
            }
            WindowCallbackEvent::Redraw => emulator.handle_event(Event::Redraw),
            WindowCallbackEvent::Keydown(x) => {
                if let Some((source, keycode)) = convert_keyboard_input(x, &keyboard_map)
                    && input_state.press(source, keycode)
                {
                    emulator.handle_event(Event::Keydown(keycode));
                }
            }
            WindowCallbackEvent::Keyup(x) => {
                if let PhysicalKey::Code(code) = x
                    && let Some(keycode) = input_state.release(InputSource::Keyboard(code))
                {
                    emulator.handle_event(Event::Keyup(keycode));
                }
            }
        }

        Ok(())
    })
}

fn config_path() -> anyhow::Result<PathBuf> {
    let exe_path = std::env::current_exe()?;
    let parent = exe_path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("Failed to determine executable directory"))?;

    Ok(parent.join("config.cfg"))
}

fn convert_keyboard_input(key: PhysicalKey, keyboard_map: &HashMap<WinitKeyCode, KeyCode>) -> Option<(InputSource, KeyCode)> {
    let PhysicalKey::Code(code) = key else {
        return None;
    };

    keyboard_map.get(&code).copied().map(|keycode| (InputSource::Keyboard(code), keycode))
}

fn handle_gamepad_event(
    emulator: &mut dyn Emulator,
    gamepad_map: &HashMap<GamepadInput, KeyCode>,
    input_state: &mut InputState,
    event: GamepadCallbackEvent,
) {
    match event {
        GamepadCallbackEvent::Keydown { id, input } => {
            let Some(keycode) = gamepad_map.get(&input).copied() else {
                return;
            };

            let source = InputSource::Gamepad { id, input };
            if input_state.press(source, keycode) {
                emulator.handle_event(Event::Keydown(keycode));
            }
        }
        GamepadCallbackEvent::Keyup { id, input } => {
            let source = InputSource::Gamepad { id, input };
            if let Some(keycode) = input_state.release(source) {
                emulator.handle_event(Event::Keyup(keycode));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::select_midi_output_port_by_name;

    #[test]
    fn midi_output_prefers_virtual_midi_synth() {
        let ports = ["Microsoft GS Wavetable Synth", "VirtualMIDISynth #1", "External MIDI"];

        assert_eq!(select_midi_output_port_by_name(ports), Some(1));
    }

    #[test]
    fn midi_output_falls_back_to_first_system_port() {
        let ports = ["Microsoft GS Wavetable Synth", "External MIDI"];

        assert_eq!(select_midi_output_port_by_name(ports), Some(0));
    }

    #[test]
    fn midi_output_returns_none_without_ports() {
        let ports: [&str; 0] = [];

        assert_eq!(select_midi_output_port_by_name(ports), None);
    }
}
