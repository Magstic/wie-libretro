use std::ffi::{c_char, c_uint, c_void};

pub const RETRO_API_VERSION: c_uint = 1;

pub const RETRO_DEVICE_JOYPAD: c_uint = 1;
pub const RETRO_DEVICE_KEYBOARD: c_uint = 3;
pub const RETRO_DEVICE_ANALOG: c_uint = 5;

pub const RETRO_DEVICE_INDEX_ANALOG_LEFT: c_uint = 0;

pub const RETRO_DEVICE_ID_ANALOG_X: c_uint = 0;
pub const RETRO_DEVICE_ID_ANALOG_Y: c_uint = 1;

pub const RETRO_DEVICE_ID_JOYPAD_B: c_uint = 0;
pub const RETRO_DEVICE_ID_JOYPAD_Y: c_uint = 1;
#[allow(dead_code)]
pub const RETRO_DEVICE_ID_JOYPAD_SELECT: c_uint = 2;
pub const RETRO_DEVICE_ID_JOYPAD_START: c_uint = 3;
pub const RETRO_DEVICE_ID_JOYPAD_UP: c_uint = 4;
pub const RETRO_DEVICE_ID_JOYPAD_DOWN: c_uint = 5;
pub const RETRO_DEVICE_ID_JOYPAD_LEFT: c_uint = 6;
pub const RETRO_DEVICE_ID_JOYPAD_RIGHT: c_uint = 7;
pub const RETRO_DEVICE_ID_JOYPAD_A: c_uint = 8;
pub const RETRO_DEVICE_ID_JOYPAD_X: c_uint = 9;
pub const RETRO_DEVICE_ID_JOYPAD_L: c_uint = 10;
pub const RETRO_DEVICE_ID_JOYPAD_R: c_uint = 11;
pub const RETRO_DEVICE_ID_JOYPAD_L2: c_uint = 12;
pub const RETRO_DEVICE_ID_JOYPAD_R2: c_uint = 13;
pub const RETROK_BACKSPACE: c_uint = 8;
pub const RETROK_RETURN: c_uint = 13;
pub const RETROK_ESCAPE: c_uint = 27;
pub const RETROK_SPACE: c_uint = 32;
pub const RETROK_HASH: c_uint = 35;
pub const RETROK_ASTERISK: c_uint = 42;
pub const RETROK_0: c_uint = 48;
pub const RETROK_1: c_uint = 49;
pub const RETROK_2: c_uint = 50;
pub const RETROK_3: c_uint = 51;
pub const RETROK_4: c_uint = 52;
pub const RETROK_5: c_uint = 53;
pub const RETROK_6: c_uint = 54;
pub const RETROK_7: c_uint = 55;
pub const RETROK_8: c_uint = 56;
pub const RETROK_9: c_uint = 57;
pub const RETROK_UP: c_uint = 273;
pub const RETROK_DOWN: c_uint = 274;
pub const RETROK_RIGHT: c_uint = 275;
pub const RETROK_LEFT: c_uint = 276;
pub const RETROK_PAGEUP: c_uint = 280;
pub const RETROK_PAGEDOWN: c_uint = 281;
pub const RETROK_F1: c_uint = 282;
pub const RETROK_F2: c_uint = 283;

pub const RETRO_ENVIRONMENT_GET_CAN_DUPE: c_uint = 3;
#[allow(dead_code)]
pub const RETRO_ENVIRONMENT_SHUTDOWN: c_uint = 7;
pub const RETRO_ENVIRONMENT_GET_SYSTEM_DIRECTORY: c_uint = 9;
pub const RETRO_ENVIRONMENT_SET_PIXEL_FORMAT: c_uint = 10;
pub const RETRO_ENVIRONMENT_SET_INPUT_DESCRIPTORS: c_uint = 11;
pub const RETRO_ENVIRONMENT_GET_VARIABLE: c_uint = 15;
pub const RETRO_ENVIRONMENT_SET_VARIABLES: c_uint = 16;
pub const RETRO_ENVIRONMENT_GET_VARIABLE_UPDATE: c_uint = 17;
pub const RETRO_ENVIRONMENT_GET_RUMBLE_INTERFACE: c_uint = 23;
pub const RETRO_ENVIRONMENT_GET_LOG_INTERFACE: c_uint = 27;
pub const RETRO_ENVIRONMENT_GET_SAVE_DIRECTORY: c_uint = 31;
pub const RETRO_ENVIRONMENT_GET_LANGUAGE: c_uint = 39;
pub const RETRO_ENVIRONMENT_GET_CORE_OPTIONS_VERSION: c_uint = 52;
pub const RETRO_ENVIRONMENT_SET_CORE_OPTIONS: c_uint = 53;
pub const RETRO_ENVIRONMENT_SET_CORE_OPTIONS_INTL: c_uint = 54;
pub const RETRO_ENVIRONMENT_SET_FASTFORWARDING_OVERRIDE: c_uint = 64;
pub const RETRO_ENVIRONMENT_GET_GAME_INFO_EXT: c_uint = 66;
pub const RETRO_ENVIRONMENT_SET_CORE_OPTIONS_V2: c_uint = 67;
pub const RETRO_ENVIRONMENT_SET_CORE_OPTIONS_V2_INTL: c_uint = 68;

pub const RETRO_LANGUAGE_CHINESE_TRADITIONAL: c_uint = 11;
pub const RETRO_LANGUAGE_CHINESE_SIMPLIFIED: c_uint = 12;

pub const RETRO_PIXEL_FORMAT_XRGB8888: c_uint = 1;
pub const RETRO_PIXEL_FORMAT_RGB565: c_uint = 2;

pub const RETRO_RUMBLE_STRONG: c_uint = 0;

pub const RETRO_LOG_INFO: c_uint = 1;
pub const RETRO_LOG_WARN: c_uint = 2;
pub const RETRO_LOG_ERROR: c_uint = 3;

pub const RETRO_REGION_NTSC: c_uint = 0;

pub const RETRO_MEMORY_SAVE_RAM: c_uint = 0;
pub const RETRO_MEMORY_RTC: c_uint = 1;
pub const RETRO_MEMORY_SYSTEM_RAM: c_uint = 2;
pub const RETRO_MEMORY_VIDEO_RAM: c_uint = 3;

pub const RETRO_NUM_CORE_OPTION_VALUES_MAX: usize = 128;

pub type RetroEnvironmentT = unsafe extern "C" fn(cmd: c_uint, data: *mut c_void) -> bool;
pub type RetroVideoRefreshT = unsafe extern "C" fn(data: *const c_void, width: c_uint, height: c_uint, pitch: usize);
pub type RetroAudioSampleT = unsafe extern "C" fn(left: i16, right: i16);
pub type RetroAudioSampleBatchT = unsafe extern "C" fn(data: *const i16, frames: usize) -> usize;
pub type RetroInputPollT = unsafe extern "C" fn();
pub type RetroInputStateT = unsafe extern "C" fn(port: c_uint, device: c_uint, index: c_uint, id: c_uint) -> i16;
pub type RetroSetRumbleStateT = unsafe extern "C" fn(port: c_uint, effect: c_uint, strength: u16) -> bool;
pub type RetroLogPrintfT = unsafe extern "C" fn(level: c_uint, fmt: *const c_char, ...);

#[repr(C)]
#[derive(Clone, Copy)]
pub struct RetroRumbleInterface {
    pub set_rumble_state: Option<RetroSetRumbleStateT>,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct RetroFastforwardingOverride {
    pub ratio: f32,
    pub fastforward: bool,
    pub notification: bool,
    pub inhibit_toggle: bool,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct RetroGameInfo {
    pub path: *const c_char,
    pub data: *const c_void,
    pub size: usize,
    pub meta: *const c_char,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct RetroGameInfoExt {
    pub full_path: *const c_char,
    pub archive_path: *const c_char,
    pub archive_file: *const c_char,
    pub dir: *const c_char,
    pub name: *const c_char,
    pub ext: *const c_char,
    pub meta: *const c_char,
    pub data: *const c_void,
    pub size: usize,
    pub file_in_archive: bool,
    pub persistent_data: bool,
}

#[repr(C)]
pub struct RetroSystemInfo {
    pub library_name: *const c_char,
    pub library_version: *const c_char,
    pub valid_extensions: *const c_char,
    pub need_fullpath: bool,
    pub block_extract: bool,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct RetroGameGeometry {
    pub base_width: c_uint,
    pub base_height: c_uint,
    pub max_width: c_uint,
    pub max_height: c_uint,
    pub aspect_ratio: f32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct RetroSystemTiming {
    pub fps: f64,
    pub sample_rate: f64,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct RetroSystemAvInfo {
    pub geometry: RetroGameGeometry,
    pub timing: RetroSystemTiming,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct RetroVariable {
    pub key: *const c_char,
    pub value: *const c_char,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct RetroCoreOptionValue {
    pub value: *const c_char,
    pub label: *const c_char,
}

impl RetroCoreOptionValue {
    pub fn empty() -> Self {
        Self {
            value: std::ptr::null(),
            label: std::ptr::null(),
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct RetroCoreOptionDefinition {
    pub key: *const c_char,
    pub desc: *const c_char,
    pub info: *const c_char,
    pub values: [RetroCoreOptionValue; RETRO_NUM_CORE_OPTION_VALUES_MAX],
    pub default_value: *const c_char,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct RetroCoreOptionV2Category {
    pub key: *const c_char,
    pub desc: *const c_char,
    pub info: *const c_char,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct RetroCoreOptionV2Definition {
    pub key: *const c_char,
    pub desc: *const c_char,
    pub desc_categorized: *const c_char,
    pub info: *const c_char,
    pub info_categorized: *const c_char,
    pub category_key: *const c_char,
    pub values: [RetroCoreOptionValue; RETRO_NUM_CORE_OPTION_VALUES_MAX],
    pub default_value: *const c_char,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct RetroCoreOptionsV2 {
    pub categories: *mut RetroCoreOptionV2Category,
    pub definitions: *mut RetroCoreOptionV2Definition,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct RetroCoreOptionsIntl {
    pub us: *mut RetroCoreOptionDefinition,
    pub local: *mut RetroCoreOptionDefinition,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct RetroCoreOptionsV2Intl {
    pub us: *mut RetroCoreOptionsV2,
    pub local: *mut RetroCoreOptionsV2,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct RetroInputDescriptor {
    pub port: c_uint,
    pub device: c_uint,
    pub index: c_uint,
    pub id: c_uint,
    pub description: *const c_char,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct RetroLogCallback {
    pub log: Option<RetroLogPrintfT>,
}
