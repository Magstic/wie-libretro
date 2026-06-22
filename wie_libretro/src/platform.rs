use std::{
    ffi::CString,
    panic,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{Ordering, Ordering::Release},
    },
    time::{SystemTime, UNIX_EPOCH},
};

use wie_backend::{AudioSink, DatabaseRepository, Filesystem, Instant, Platform, Screen, canvas::Image};
use wie_util::Result;

use crate::{
    audio::LibretroAudioSink,
    database::LibretroDatabaseRepository,
    ffi::{RETRO_LOG_ERROR, RETRO_LOG_INFO, RetroLogPrintfT},
    filesystem::LibretroFilesystem,
    shared::{GuestExit, Shared},
};

#[derive(Clone, Copy)]
pub struct LogInterface {
    pub log: Option<RetroLogPrintfT>,
}

pub struct LibretroPlatform {
    screen: LibretroScreen,
    filesystem: LibretroFilesystem,
    database_repository: LibretroDatabaseRepository,
    shared: Arc<Shared>,
    log: LogInterface,
}

impl LibretroPlatform {
    pub fn new(width: u32, height: u32, save_dir: PathBuf, shared: Arc<Shared>, log: LogInterface) -> Self {
        Self {
            screen: LibretroScreen {
                width,
                height,
                shared: shared.clone(),
            },
            filesystem: LibretroFilesystem::new(save_dir.clone()),
            database_repository: LibretroDatabaseRepository::new(save_dir),
            shared,
            log,
        }
    }
}

impl Platform for LibretroPlatform {
    fn screen(&self) -> &dyn Screen {
        &self.screen
    }

    fn now(&self) -> Instant {
        let now = SystemTime::now();
        let since_the_epoch = now.duration_since(UNIX_EPOCH).unwrap_or_default();

        Instant::from_epoch_millis(since_the_epoch.as_millis() as _)
    }

    fn database_repository(&self) -> &dyn DatabaseRepository {
        &self.database_repository
    }

    fn filesystem(&self) -> &dyn Filesystem {
        &self.filesystem
    }

    fn audio_sink(&self) -> Box<dyn AudioSink> {
        Box::new(LibretroAudioSink::new(self.shared.audio.clone(), self.shared.midi.clone()))
    }

    fn write_stdout(&self, buf: &[u8]) {
        self.log.write(RETRO_LOG_INFO, buf);
    }

    fn write_stderr(&self, buf: &[u8]) {
        self.log.write(RETRO_LOG_ERROR, buf);
    }

    fn exit(&self) {
        self.shared.quit.store(true, Release);
        panic::resume_unwind(Box::new(GuestExit));
    }

    fn vibrate(&self, duration_ms: u64, intensity: u8) {
        if let Ok(mut rumble) = self.shared.rumble.lock() {
            *rumble = Some((duration_ms, intensity));
        }
    }
}

impl LogInterface {
    pub fn write(&self, level: u32, bytes: &[u8]) {
        let message = String::from_utf8_lossy(bytes);
        let message = message.replace('\0', " ");

        if let Some(log) = self.log {
            let Ok(message) = CString::new(message.as_bytes()) else {
                return;
            };
            unsafe {
                log(level, c"%s".as_ptr(), message.as_ptr());
            }
        } else if level == RETRO_LOG_ERROR {
            eprintln!("{message}");
        } else {
            eprint!("{message}");
        }
    }
}

pub struct LibretroScreen {
    width: u32,
    height: u32,
    shared: Arc<Shared>,
}

impl Screen for LibretroScreen {
    fn request_redraw(&self) -> Result<()> {
        self.shared.redraw_requested.store(true, Ordering::Release);
        Ok(())
    }

    fn paint(&self, image: &dyn Image) {
        if let Ok(mut framebuffer) = self.shared.framebuffer.lock() {
            framebuffer.replace_argb8888(image.width(), image.height(), image.argb8888());
        }
    }

    fn width(&self) -> u32 {
        self.width
    }

    fn height(&self) -> u32 {
        self.height
    }
}
