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
    fs::{self, File},
    io::{LineWriter, Write, stderr},
    path::PathBuf,
    sync::{Mutex, mpsc::Sender, mpsc::channel},
    thread,
    time::{SystemTime, UNIX_EPOCH},
};

use clap::Parser;
use gilrs::GamepadId;
use midir::{MidiOutput, MidiOutputPort};
use winit::keyboard::{KeyCode as WinitKeyCode, PhysicalKey};

use wie_backend::{AudioCommand, Emulator, Event, Filesystem, Instant, KeyCode, Options, Platform, ProfileSample, Screen, extract_zip};
use wie_j2me::J2MEEmulator;
use wie_ktf::KtfEmulator;
use wie_lgt::LgtEmulator;
use wie_skt::SktEmulator;
use wie_util::WieError;

use self::{
    audio_sink::AudioSink,
    config::{Config, GamepadInput},
    database::DatabaseRepository,
    filesystem::CliFilesystem,
    gamepad::{GamepadCallbackEvent, GamepadState},
    window::{WindowCallbackEvent, WindowHandle, WindowImpl},
};

struct WieCliPlatform {
    audio_tx: Sender<AudioCommand>,
    database_repository: DatabaseRepository,
    filesystem: CliFilesystem,
    vibrate_tx: Sender<(u64, u8)>,
    window: WindowHandle,
}

enum EmulatorCommand {
    Event(Event),
    Shutdown,
}

impl WieCliPlatform {
    fn new(window: WindowHandle, vibrate_tx: Sender<(u64, u8)>, midi_device: Option<usize>) -> Self {
        let (tx, rx) = channel();
        thread::spawn(move || audio_sink::run(rx, midi_device));

        Self {
            audio_tx: tx,
            database_repository: DatabaseRepository::new(),
            filesystem: CliFilesystem::new(),
            vibrate_tx,
            window,
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
        Box::new(AudioSink::new(self.audio_tx.clone()))
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

fn select_midi_output_port(midi_out: &MidiOutput, midi_ports: &[MidiOutputPort], requested: Option<usize>) -> Option<usize> {
    requested
        .filter(|&index| index < midi_ports.len())
        .or_else(|| select_midi_output_port_by_name(midi_ports.iter().map(|port| midi_out.port_name(port).unwrap_or_default())))
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
    #[arg(required_unless_present = "list_midi_devices")]
    filename: Option<String>,
    #[arg(long, default_value_t = false)]
    debug: bool,
    /// Write a flamegraph-folded sampling profile to this path (one line per
    /// flushed batch; `flamegraph.pl` aggregates duplicates).
    #[arg(long)]
    profile_out: Option<PathBuf>,
    /// Select a MIDI output by zero-based index.
    #[arg(long, value_name = "INDEX")]
    midi_device: Option<usize>,
    /// List available MIDI output devices and exit.
    #[arg(long, conflicts_with_all = ["filename", "debug", "profile_out", "midi_device"])]
    list_midi_devices: bool,
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_writer(stderr)
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let args = Args::parse();

    if args.list_midi_devices {
        return list_midi_devices();
    }

    let profile = args.profile_out.as_ref().map(|path| profile_callback(path)).transpose()?;
    let options = Options {
        enable_gdbserver: args.debug,
        profile,
    };
    let filename = args.filename.as_deref().ok_or_else(|| anyhow::anyhow!("filename is required"))?;

    start_with_midi_device(filename, options, args.midi_device)
}

fn list_midi_devices() -> anyhow::Result<()> {
    let midi_out = MidiOutput::new("wie_cli")?;
    for (index, port) in midi_out.ports().iter().enumerate() {
        let name = midi_out.port_name(port).unwrap_or_else(|_| "<unknown>".to_string());
        println!("{index}: {name}");
    }
    Ok(())
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
    start_with_midi_device(filename, options, None)
}

fn start_with_midi_device(filename: &str, options: Options, midi_device: Option<usize>) -> anyhow::Result<()> {
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
    let window = WindowImpl::new(240, 320)?; // TODO hardcoded size
    let window_handle = window.handle();
    let platform = Box::new(WieCliPlatform::new(window_handle.clone(), vibrate_tx, midi_device));

    let buf = fs::read(filename)?;
    let mut emulator: Box<dyn Emulator + Send> = if filename.ends_with("zip") {
        let files = extract_zip(&buf)?;

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

    let (emulator_tx, emulator_rx) = channel();
    let emulator_window = window_handle.clone();
    let emulator_thread = thread::Builder::new()
        .name("wie-emulator".into())
        .spawn(move || -> wie_util::Result<()> {
            loop {
                let mut shutdown = false;
                for command in emulator_rx.try_iter() {
                    match command {
                        EmulatorCommand::Event(event) => emulator.handle_event(event),
                        EmulatorCommand::Shutdown => shutdown = true,
                    }
                }
                if shutdown {
                    return Ok(());
                }

                if let Err(error) = emulator.tick() {
                    emulator_window.send_quit_event();
                    return Err(error);
                }
            }
        })?;

    let mut input_state = InputState::default();
    let callback_tx = emulator_tx.clone();
    let window_result = window.run(move |event| {
        match event {
            WindowCallbackEvent::Update => {
                let now = SystemTime::now();

                if let Some(gamepad) = gamepad.as_mut() {
                    for gamepad_event in gamepad.poll() {
                        handle_gamepad_event(&callback_tx, &gamepad_map, &mut input_state, gamepad_event)?;
                    }
                }

                for keycode in input_state.repeat_due(now) {
                    send_emulator_event(&callback_tx, Event::Keyrepeat(keycode))?;
                }
            }
            WindowCallbackEvent::Redraw => send_emulator_event(&callback_tx, Event::Redraw)?,
            WindowCallbackEvent::Keydown(x) => {
                if let Some((source, keycode)) = convert_keyboard_input(x, &keyboard_map)
                    && input_state.press(source, keycode)
                {
                    send_emulator_event(&callback_tx, Event::Keydown(keycode))?;
                }
            }
            WindowCallbackEvent::Keyup(x) => {
                if let PhysicalKey::Code(code) = x
                    && let Some(keycode) = input_state.release(InputSource::Keyboard(code))
                {
                    send_emulator_event(&callback_tx, Event::Keyup(keycode))?;
                }
            }
        }

        Ok(())
    });

    let _ = emulator_tx.send(EmulatorCommand::Shutdown);
    let emulator_result = emulator_thread.join().map_err(|_| anyhow::anyhow!("Emulator thread panicked"))?;
    window_result?;
    emulator_result.map_err(anyhow::Error::from)
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

fn send_emulator_event(sender: &Sender<EmulatorCommand>, event: Event) -> wie_util::Result<()> {
    sender
        .send(EmulatorCommand::Event(event))
        .map_err(|_| WieError::FatalError("Emulator thread is not running".into()))
}

fn handle_gamepad_event(
    emulator_tx: &Sender<EmulatorCommand>,
    gamepad_map: &HashMap<GamepadInput, KeyCode>,
    input_state: &mut InputState,
    event: GamepadCallbackEvent,
) -> wie_util::Result<()> {
    match event {
        GamepadCallbackEvent::Keydown { id, input } => {
            let Some(keycode) = gamepad_map.get(&input).copied() else {
                return Ok(());
            };

            let source = InputSource::Gamepad { id, input };
            if input_state.press(source, keycode) {
                send_emulator_event(emulator_tx, Event::Keydown(keycode))?;
            }
        }
        GamepadCallbackEvent::Keyup { id, input } => {
            let source = InputSource::Gamepad { id, input };
            if let Some(keycode) = input_state.release(source) {
                send_emulator_event(emulator_tx, Event::Keyup(keycode))?;
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::{Args, select_midi_output_port_by_name};

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

    #[test]
    fn list_midi_devices_does_not_require_filename() {
        let args = Args::try_parse_from(["wie_cli", "--list-midi-devices"]).unwrap();

        assert!(args.list_midi_devices);
        assert!(args.filename.is_none());
    }

    #[test]
    fn normal_run_requires_filename() {
        assert!(Args::try_parse_from(["wie_cli"]).is_err());
    }

    #[test]
    fn parses_midi_device_with_filename() {
        let args = Args::try_parse_from(["wie_cli", "game.jar", "--midi-device", "1"]).unwrap();

        assert_eq!(args.filename.as_deref(), Some("game.jar"));
        assert_eq!(args.midi_device, Some(1));
    }
}
