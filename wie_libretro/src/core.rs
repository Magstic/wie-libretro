use std::{
    ffi::c_void,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{Ordering::Acquire, Ordering::Release},
        mpsc::{self, Receiver, Sender},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant as StdInstant},
};

use wie_backend::{Emulator, Event, Platform};
use wie_util::{Result, WieError};

use crate::{
    audio::{AUDIO_FPS, AUDIO_SAMPLE_RATE, AudioState, MidiOutput},
    content::{LoadedContent, load_emulator},
    environment::{CoreOptions, core_options_updated, read_core_options},
    ffi::{
        RETRO_ENVIRONMENT_GET_CAN_DUPE, RETRO_ENVIRONMENT_GET_RUMBLE_INTERFACE, RETRO_ENVIRONMENT_SET_INPUT_DESCRIPTORS,
        RETRO_ENVIRONMENT_SET_PIXEL_FORMAT, RETRO_PIXEL_FORMAT_RGB565, RETRO_PIXEL_FORMAT_XRGB8888, RETRO_RUMBLE_STRONG, RetroAudioSampleBatchT,
        RetroAudioSampleT, RetroEnvironmentT, RetroInputPollT, RetroInputStateT, RetroRumbleInterface, RetroSetRumbleStateT, RetroVideoRefreshT,
    },
    input::{InputManager, input_descriptors},
    platform::LogInterface,
    shared::{GuestExit, Shared},
    video::Frame,
};

const WORKER_STACK_SIZE: usize = 32 * 1024 * 1024;
// Avoid a busy loop when all emulator tasks are sleeping.
const IDLE_THROTTLE: Duration = Duration::from_millis(1);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PixelFormat {
    Xrgb8888,
    Rgb565,
}

pub enum WorkerMsg {
    Event(Event),
}

#[derive(Clone, Copy)]
pub struct CoreStartCallbacks {
    pub environ: Option<RetroEnvironmentT>,
    pub log: LogInterface,
}

#[derive(Clone, Copy)]
pub struct RunCallbacks {
    pub environ: Option<RetroEnvironmentT>,
    pub video_refresh: Option<RetroVideoRefreshT>,
    pub audio_sample: Option<RetroAudioSampleT>,
    pub audio_sample_batch: Option<RetroAudioSampleBatchT>,
    pub input_poll: Option<RetroInputPollT>,
    pub input_state: Option<RetroInputStateT>,
    pub log: LogInterface,
}

pub struct LibretroCore {
    shared: Arc<Shared>,
    tx: Sender<WorkerMsg>,
    worker: Option<JoinHandle<()>>,
    input: InputManager,
    content: LoadedContent,
    options: CoreOptions,
    pixel_format: PixelFormat,
    video_frame: Frame,
    audio_frames_per_run: usize,
    audio_buffer: Vec<i16>,
    set_rumble_state: Option<RetroSetRumbleStateT>,
    rumble_deadline: Option<StdInstant>,
}

impl LibretroCore {
    pub fn load(content: LoadedContent, options: CoreOptions, save_dir: PathBuf, callbacks: CoreStartCallbacks) -> Result<Self> {
        let pixel_format = set_pixel_format(callbacks.environ);
        set_input_descriptors(callbacks.environ);

        let audio = Arc::new(Mutex::new(AudioState::new(AUDIO_SAMPLE_RATE)));
        let midi = Arc::new(MidiOutput::new(
            options.midi_enabled,
            options.sound_font_path.clone(),
            options.midi_volume,
        ));
        let shared = Arc::new(Shared::new(options.width, options.height, audio, midi));
        let platform: Box<dyn Platform> = Box::new(crate::platform::LibretroPlatform::new(
            options.width,
            options.height,
            save_dir,
            shared.clone(),
            callbacks.log,
        ));
        wie_core_arm::set_hooks_enabled(options.hooks_enabled);
        let emulator = load_emulator(platform, &content, options.runtime)?;

        let (tx, rx) = mpsc::channel();
        let worker_shared = shared.clone();
        let worker = thread::Builder::new()
            .name("wie-libretro-emu".into())
            .stack_size(WORKER_STACK_SIZE)
            .spawn(move || {
                let shared = worker_shared.clone();
                match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| worker_main(emulator, rx, worker_shared))) {
                    Ok(()) => {}
                    Err(payload) if payload.is::<GuestExit>() => {
                        shared.quit.store(true, Release);
                    }
                    Err(_) => {
                        if let Ok(mut fatal) = shared.fatal.lock() {
                            *fatal = Some("emulator worker panicked".to_owned());
                        }
                        shared.quit.store(true, Release);
                    }
                }
            })
            .map_err(|err| WieError::FatalError(format!("Failed to start libretro worker thread: {err}")))?;

        let audio_frames_per_run = (AUDIO_SAMPLE_RATE as f64 / AUDIO_FPS).round() as usize;
        let video_frame = Frame::new(options.width, options.height);
        Ok(Self {
            shared,
            tx,
            worker: Some(worker),
            input: InputManager::new(),
            content,
            options,
            pixel_format,
            video_frame,
            audio_frames_per_run,
            audio_buffer: vec![0; audio_frames_per_run * 2],
            set_rumble_state: get_rumble_interface(callbacks.environ),
            rumble_deadline: None,
        })
    }

    pub fn run(&mut self, callbacks: RunCallbacks) {
        if core_options_updated(callbacks.environ) {
            self.apply_hot_options(callbacks);
        }

        for event in self.input.poll(callbacks.input_poll, callbacks.input_state) {
            self.send_event(event);
        }

        self.update_rumble();
        self.present_video(callbacks.video_refresh);
        self.present_audio(callbacks.audio_sample, callbacks.audio_sample_batch);

        if self.shared.quit.load(Acquire)
            && let Ok(mut fatal) = self.shared.fatal.lock()
            && let Some(message) = fatal.take()
        {
            callbacks.log.write(crate::ffi::RETRO_LOG_ERROR, message.as_bytes());
        }
    }

    pub fn content(&self) -> LoadedContent {
        self.content.clone()
    }

    pub fn shutdown(&mut self) {
        self.stop_rumble();
        self.shared.midi.shutdown();
        if let Ok(mut audio) = self.shared.audio.lock() {
            audio.clear();
        }
        self.shared.quit.store(true, Release);

        if let Some(worker) = self.worker.take()
            && worker.join().is_err()
            && let Ok(mut fatal) = self.shared.fatal.lock()
        {
            *fatal = Some("emulator worker panicked during shutdown".to_owned());
        }
        if let Ok(mut audio) = self.shared.audio.lock() {
            audio.clear();
        }
    }

    fn apply_hot_options(&mut self, callbacks: RunCallbacks) {
        let new_options = read_core_options(callbacks.environ);
        if self.options.midi_enabled != new_options.midi_enabled {
            self.shared.midi.set_enabled(new_options.midi_enabled);
        }

        if self.options.hooks_enabled != new_options.hooks_enabled {
            wie_core_arm::set_hooks_enabled(new_options.hooks_enabled);
        }

        if self.options.width != new_options.width
            || self.options.height != new_options.height
            || self.options.runtime != new_options.runtime
            || self.options.midi_volume != new_options.midi_volume
            || self.options.sound_font_path != new_options.sound_font_path
            || self.options.hooks_enabled != new_options.hooks_enabled
        {
            callbacks.log.write(
                crate::ffi::RETRO_LOG_WARN,
                b"Some wie core option changes require reloading the content to take effect.",
            );
        }

        self.options = new_options;
    }

    fn send_event(&self, event: Event) {
        if self.tx.send(WorkerMsg::Event(event)).is_err() {
            self.shared.quit.store(true, Release);
        }
    }

    fn update_rumble(&mut self) {
        let request = self.shared.rumble.lock().ok().and_then(|mut rumble| rumble.take());
        let Some(set_rumble_state) = self.set_rumble_state else {
            return;
        };

        if let Some((duration_ms, intensity)) = request {
            let intensity = intensity.min(100);
            let strength = (u32::from(intensity) * u32::from(u16::MAX) / 100) as u16;
            unsafe {
                set_rumble_state(0, RETRO_RUMBLE_STRONG, strength);
            }
            self.rumble_deadline = if strength == 0 || duration_ms == 0 {
                None
            } else {
                let now = StdInstant::now();
                Some(now.checked_add(Duration::from_millis(duration_ms)).unwrap_or(now))
            };
        }

        if self.rumble_deadline.is_some_and(|deadline| StdInstant::now() >= deadline) {
            self.stop_rumble();
        }
    }

    fn stop_rumble(&mut self) {
        if self.rumble_deadline.take().is_some()
            && let Some(set_rumble_state) = self.set_rumble_state
        {
            unsafe {
                set_rumble_state(0, RETRO_RUMBLE_STRONG, 0);
            }
        }
    }

    fn present_video(&mut self, video_refresh: Option<RetroVideoRefreshT>) {
        let Some(video_refresh) = video_refresh else {
            return;
        };

        if let Ok(frame) = self.shared.framebuffer.lock() {
            self.video_frame.clone_from(&frame);
        }

        match self.pixel_format {
            PixelFormat::Xrgb8888 => unsafe {
                video_refresh(
                    self.video_frame.argb8888.as_ptr().cast::<c_void>(),
                    self.video_frame.width,
                    self.video_frame.height,
                    self.video_frame.width as usize * 4,
                );
            },
            PixelFormat::Rgb565 => {
                let width = self.video_frame.width;
                let height = self.video_frame.height;
                let pixels = self.video_frame.rgb565();
                unsafe {
                    video_refresh(pixels.as_ptr().cast::<c_void>(), width, height, width as usize * 2);
                }
            }
        }
    }

    fn present_audio(&mut self, audio_sample: Option<RetroAudioSampleT>, audio_sample_batch: Option<RetroAudioSampleBatchT>) {
        if let Ok(mut audio) = self.shared.audio.lock() {
            audio.render(&mut self.audio_buffer);
        } else {
            self.audio_buffer.fill(0);
        }
        self.shared.midi.render_into(&mut self.audio_buffer);

        if let Some(audio_sample_batch) = audio_sample_batch {
            unsafe {
                audio_sample_batch(self.audio_buffer.as_ptr(), self.audio_frames_per_run);
            }
        } else if let Some(audio_sample) = audio_sample {
            for frame in self.audio_buffer.chunks_exact(2) {
                unsafe { audio_sample(frame[0], frame[1]) };
            }
        }
    }
}

impl Drop for LibretroCore {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn worker_main(mut emulator: Box<dyn Emulator + Send>, rx: Receiver<WorkerMsg>, shared: Arc<Shared>) {
    loop {
        if shared.quit.load(Acquire) {
            break;
        }

        for msg in rx.try_iter() {
            match msg {
                WorkerMsg::Event(event) => emulator.handle_event(event),
            }
        }

        if shared.redraw_requested.swap(false, std::sync::atomic::Ordering::AcqRel) {
            emulator.handle_event(Event::Redraw);
        }

        let tick_start = StdInstant::now();
        if let Err(err) = emulator.tick() {
            if let Ok(mut fatal) = shared.fatal.lock() {
                *fatal = Some(err.to_string());
            }
            shared.quit.store(true, Release);
            break;
        }
        let elapsed = tick_start.elapsed();
        if elapsed < IDLE_THROTTLE {
            thread::sleep(IDLE_THROTTLE - elapsed);
        }
    }
}

fn set_pixel_format(environ: Option<RetroEnvironmentT>) -> PixelFormat {
    let Some(environ) = environ else {
        return PixelFormat::Xrgb8888;
    };

    let mut format = RETRO_PIXEL_FORMAT_XRGB8888;
    let ok = unsafe { environ(RETRO_ENVIRONMENT_SET_PIXEL_FORMAT, (&mut format as *mut u32).cast::<c_void>()) };
    if ok {
        return PixelFormat::Xrgb8888;
    }

    format = RETRO_PIXEL_FORMAT_RGB565;
    if unsafe { environ(RETRO_ENVIRONMENT_SET_PIXEL_FORMAT, (&mut format as *mut u32).cast::<c_void>()) } {
        return PixelFormat::Rgb565;
    }

    PixelFormat::Xrgb8888
}

fn get_rumble_interface(environ: Option<RetroEnvironmentT>) -> Option<RetroSetRumbleStateT> {
    let environ = environ?;
    let mut rumble = RetroRumbleInterface { set_rumble_state: None };
    if unsafe {
        environ(
            RETRO_ENVIRONMENT_GET_RUMBLE_INTERFACE,
            (&mut rumble as *mut RetroRumbleInterface).cast::<c_void>(),
        )
    } {
        rumble.set_rumble_state
    } else {
        None
    }
}

fn set_input_descriptors(environ: Option<RetroEnvironmentT>) {
    let Some(environ) = environ else {
        return;
    };

    let descriptors = Box::leak(input_descriptors().into_boxed_slice());
    unsafe {
        environ(RETRO_ENVIRONMENT_SET_INPUT_DESCRIPTORS, descriptors.as_mut_ptr().cast::<c_void>());
    }
}

pub fn can_dupe(environ: Option<RetroEnvironmentT>) -> bool {
    let Some(environ) = environ else {
        return false;
    };

    let mut can_dupe = false;
    (unsafe { environ(RETRO_ENVIRONMENT_GET_CAN_DUPE, (&mut can_dupe as *mut bool).cast::<c_void>()) }) && can_dupe
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc, Mutex,
        atomic::{AtomicU16, AtomicUsize, Ordering},
        mpsc,
    };
    use std::time::Instant;

    use crate::{
        audio::{AudioState, MidiOutput},
        content::LoadedContent,
        environment::CoreOptions,
        ffi::{RETRO_ENVIRONMENT_SHUTDOWN, RetroEnvironmentT, RetroVideoRefreshT},
        video::Frame,
    };

    use super::{LibretroCore, PixelFormat, RunCallbacks};

    static SHUTDOWN_REQUESTS: AtomicUsize = AtomicUsize::new(0);
    static RUMBLE_STRENGTH: AtomicU16 = AtomicU16::new(0);
    static VIDEO_CALLBACK_FRAMEBUFFER_UNLOCKED: AtomicUsize = AtomicUsize::new(0);
    static VIDEO_TEST_SHARED: Mutex<Option<Arc<crate::shared::Shared>>> = Mutex::new(None);

    #[test]
    fn quit_state_does_not_request_frontend_shutdown() {
        SHUTDOWN_REQUESTS.store(0, Ordering::SeqCst);

        let (tx, _rx) = mpsc::channel();
        let options = CoreOptions::default();
        let shared = Arc::new(crate::shared::Shared::new(
            options.width,
            options.height,
            Arc::new(Mutex::new(AudioState::new(crate::audio::AUDIO_SAMPLE_RATE))),
            Arc::new(MidiOutput::new(false, None, 5)),
        ));
        shared.quit.store(true, Ordering::Release);

        let mut core = LibretroCore {
            shared,
            tx,
            worker: None,
            input: crate::input::InputManager::new(),
            content: LoadedContent::single("test.zip".to_owned(), Vec::new()),
            options,
            pixel_format: PixelFormat::Xrgb8888,
            video_frame: Frame::new(1, 1),
            audio_frames_per_run: 1,
            audio_buffer: vec![0; 2],
            set_rumble_state: None,
            rumble_deadline: None,
        };

        core.run(RunCallbacks {
            environ: Some(test_environment as RetroEnvironmentT),
            video_refresh: None,
            audio_sample: None,
            audio_sample_batch: None,
            input_poll: None,
            input_state: None,
            log: crate::platform::LogInterface { log: None },
        });

        assert_eq!(SHUTDOWN_REQUESTS.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn video_callback_does_not_hold_worker_framebuffer_lock() {
        VIDEO_CALLBACK_FRAMEBUFFER_UNLOCKED.store(0, Ordering::SeqCst);

        let (tx, _rx) = mpsc::channel();
        let options = CoreOptions::default();
        let shared = Arc::new(crate::shared::Shared::new(
            options.width,
            options.height,
            Arc::new(Mutex::new(AudioState::new(crate::audio::AUDIO_SAMPLE_RATE))),
            Arc::new(MidiOutput::new(false, None, 5)),
        ));
        *VIDEO_TEST_SHARED.lock().unwrap() = Some(shared.clone());

        let mut core = LibretroCore {
            shared,
            tx,
            worker: None,
            input: crate::input::InputManager::new(),
            content: LoadedContent::single("test.zip".to_owned(), Vec::new()),
            options,
            pixel_format: PixelFormat::Xrgb8888,
            video_frame: Frame::new(1, 1),
            audio_frames_per_run: 1,
            audio_buffer: vec![0; 2],
            set_rumble_state: None,
            rumble_deadline: None,
        };

        core.present_video(Some(test_video_refresh as RetroVideoRefreshT));

        assert_eq!(VIDEO_CALLBACK_FRAMEBUFFER_UNLOCKED.load(Ordering::SeqCst), 1);
        *VIDEO_TEST_SHARED.lock().unwrap() = None;
    }

    #[test]
    fn rumble_request_starts_and_stops() {
        RUMBLE_STRENGTH.store(0, Ordering::SeqCst);

        let (tx, _rx) = mpsc::channel();
        let options = CoreOptions::default();
        let shared = Arc::new(crate::shared::Shared::new(
            options.width,
            options.height,
            Arc::new(Mutex::new(AudioState::new(crate::audio::AUDIO_SAMPLE_RATE))),
            Arc::new(MidiOutput::new(false, None, 5)),
        ));
        *shared.rumble.lock().unwrap() = Some((1000, 50));

        let mut core = LibretroCore {
            shared,
            tx,
            worker: None,
            input: crate::input::InputManager::new(),
            content: LoadedContent::single("test.zip".to_owned(), Vec::new()),
            options,
            pixel_format: PixelFormat::Xrgb8888,
            video_frame: Frame::new(1, 1),
            audio_frames_per_run: 1,
            audio_buffer: vec![0; 2],
            set_rumble_state: Some(test_set_rumble_state),
            rumble_deadline: None,
        };

        core.update_rumble();
        assert_eq!(RUMBLE_STRENGTH.load(Ordering::SeqCst), 32767);

        core.rumble_deadline = Some(Instant::now());
        core.update_rumble();
        assert_eq!(RUMBLE_STRENGTH.load(Ordering::SeqCst), 0);
    }

    unsafe extern "C" fn test_environment(cmd: std::ffi::c_uint, _data: *mut std::ffi::c_void) -> bool {
        if cmd == RETRO_ENVIRONMENT_SHUTDOWN {
            SHUTDOWN_REQUESTS.fetch_add(1, Ordering::SeqCst);
            true
        } else {
            false
        }
    }

    unsafe extern "C" fn test_video_refresh(_data: *const std::ffi::c_void, _width: u32, _height: u32, _pitch: usize) {
        let shared = VIDEO_TEST_SHARED.lock().unwrap().clone().unwrap();
        if shared.framebuffer.try_lock().is_ok() {
            VIDEO_CALLBACK_FRAMEBUFFER_UNLOCKED.store(1, Ordering::SeqCst);
        }
    }

    unsafe extern "C" fn test_set_rumble_state(_port: u32, _effect: u32, strength: u16) -> bool {
        RUMBLE_STRENGTH.store(strength, Ordering::SeqCst);
        true
    }
}
