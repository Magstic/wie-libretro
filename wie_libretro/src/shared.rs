use std::sync::{Arc, Mutex, atomic::AtomicBool};

use crate::{
    audio::{AudioState, MidiOutput},
    video::Frame,
};

#[derive(Debug)]
pub struct GuestExit;

pub struct Shared {
    pub framebuffer: Mutex<Frame>,
    pub redraw_requested: AtomicBool,
    pub audio: Arc<Mutex<AudioState>>,
    pub midi: Arc<MidiOutput>,
    pub rumble: Mutex<Option<(u64, u8)>>,
    pub quit: AtomicBool,
    pub fatal: Mutex<Option<String>>,
}

impl Shared {
    pub fn new(width: u32, height: u32, audio: Arc<Mutex<AudioState>>, midi: Arc<MidiOutput>) -> Self {
        Self {
            framebuffer: Mutex::new(Frame::new(width, height)),
            redraw_requested: AtomicBool::new(true),
            audio,
            midi,
            rumble: Mutex::new(None),
            quit: AtomicBool::new(false),
            fatal: Mutex::new(None),
        }
    }
}
