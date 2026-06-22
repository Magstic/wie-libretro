mod audio;
mod content;
mod core;
mod database;
mod environment;
mod ffi;
mod filesystem;
mod input;
mod platform;
mod shared;
mod video;

use std::{
    ffi::{CStr, c_uint, c_void},
    panic::{AssertUnwindSafe, catch_unwind},
    path::{Path, PathBuf},
    ptr, slice,
    sync::{LazyLock, Mutex},
};

use content::LoadedContent;
use core::{CoreStartCallbacks, LibretroCore, RunCallbacks};
use environment::{CoreOptions, get_directories, read_core_options, register_environment};
use ffi::{
    RETRO_API_VERSION, RETRO_ENVIRONMENT_GET_LOG_INTERFACE, RETRO_MEMORY_RTC, RETRO_MEMORY_SAVE_RAM, RETRO_MEMORY_SYSTEM_RAM, RETRO_MEMORY_VIDEO_RAM,
    RETRO_REGION_NTSC, RetroAudioSampleBatchT, RetroAudioSampleT, RetroEnvironmentT, RetroGameGeometry, RetroGameInfo, RetroGameInfoExt,
    RetroInputPollT, RetroInputStateT, RetroLogCallback, RetroSystemAvInfo, RetroSystemInfo, RetroSystemTiming, RetroVideoRefreshT,
};
use platform::LogInterface;

static STATE: LazyLock<Mutex<GlobalState>> = LazyLock::new(|| Mutex::new(GlobalState::default()));

struct GlobalState {
    environ: Option<RetroEnvironmentT>,
    video_refresh: Option<RetroVideoRefreshT>,
    audio_sample: Option<RetroAudioSampleT>,
    audio_sample_batch: Option<RetroAudioSampleBatchT>,
    input_poll: Option<RetroInputPollT>,
    input_state: Option<RetroInputStateT>,
    log: LogInterface,
    save_dir: PathBuf,
    core: Option<LibretroCore>,
}

impl Default for GlobalState {
    fn default() -> Self {
        let save_dir = std::env::current_dir().unwrap_or_else(|_| std::env::temp_dir()).join("wie");

        Self {
            environ: None,
            video_refresh: None,
            audio_sample: None,
            audio_sample_batch: None,
            input_poll: None,
            input_state: None,
            log: LogInterface { log: None },
            save_dir,
            core: None,
        }
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn retro_set_environment(cb: RetroEnvironmentT) {
    ffi_guard(|| {
        if let Ok(mut state) = STATE.lock() {
            state.environ = Some(cb);
        }
        register_environment(cb);
    });
}

#[unsafe(no_mangle)]
pub extern "C" fn retro_set_video_refresh(cb: RetroVideoRefreshT) {
    ffi_guard(|| {
        if let Ok(mut state) = STATE.lock() {
            state.video_refresh = Some(cb);
        }
    });
}

#[unsafe(no_mangle)]
pub extern "C" fn retro_set_audio_sample(cb: RetroAudioSampleT) {
    ffi_guard(|| {
        if let Ok(mut state) = STATE.lock() {
            state.audio_sample = Some(cb);
        }
    });
}

#[unsafe(no_mangle)]
pub extern "C" fn retro_set_audio_sample_batch(cb: RetroAudioSampleBatchT) {
    ffi_guard(|| {
        if let Ok(mut state) = STATE.lock() {
            state.audio_sample_batch = Some(cb);
        }
    });
}

#[unsafe(no_mangle)]
pub extern "C" fn retro_set_input_poll(cb: RetroInputPollT) {
    ffi_guard(|| {
        if let Ok(mut state) = STATE.lock() {
            state.input_poll = Some(cb);
        }
    });
}

#[unsafe(no_mangle)]
pub extern "C" fn retro_set_input_state(cb: RetroInputStateT) {
    ffi_guard(|| {
        if let Ok(mut state) = STATE.lock() {
            state.input_state = Some(cb);
        }
    });
}

#[unsafe(no_mangle)]
pub extern "C" fn retro_init() {
    ffi_guard(|| {
        if let Ok(mut state) = STATE.lock() {
            state.save_dir = get_directories(state.environ);
            state.log = get_log_interface(state.environ);
            let _ = crate::core::can_dupe(state.environ);
        }
    });
}

#[unsafe(no_mangle)]
pub extern "C" fn retro_deinit() {
    ffi_guard(|| {
        if let Ok(mut state) = STATE.lock() {
            if let Some(mut core) = state.core.take() {
                core.shutdown();
            }
            *state = GlobalState::default();
        }
    });
}

#[unsafe(no_mangle)]
pub extern "C" fn retro_api_version() -> c_uint {
    RETRO_API_VERSION
}

#[unsafe(no_mangle)]
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn retro_get_system_info(info: *mut RetroSystemInfo) {
    ffi_guard(|| {
        if info.is_null() {
            return;
        }

        unsafe {
            *info = RetroSystemInfo {
                library_name: c"Mobile - Korea (Wie-Libretro)".as_ptr(),
                library_version: c"0.0.1".as_ptr(),
                valid_extensions: c"zip".as_ptr(),
                need_fullpath: true,
                block_extract: true,
            };
        }
    });
}

#[unsafe(no_mangle)]
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn retro_get_system_av_info(info: *mut RetroSystemAvInfo) {
    ffi_guard(|| {
        if info.is_null() {
            return;
        }

        let options = STATE.lock().ok().map(|state| read_core_options(state.environ)).unwrap_or_default();
        unsafe {
            *info = av_info(&options);
        }
    });
}

#[unsafe(no_mangle)]
pub extern "C" fn retro_set_controller_port_device(_port: c_uint, _device: c_uint) {}

#[unsafe(no_mangle)]
pub extern "C" fn retro_reset() {
    ffi_guard(|| {
        if let Ok(mut state) = STATE.lock() {
            let Some(content) = state.core.as_ref().map(LibretroCore::content) else {
                return;
            };
            unload_locked(&mut state);
            let _ = load_locked(&mut state, content);
        }
    });
}

#[unsafe(no_mangle)]
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn retro_load_game(game: *const RetroGameInfo) -> bool {
    ffi_guard_bool(|| {
        if game.is_null() {
            log_global(ffi::RETRO_LOG_ERROR, b"retro_load_game: game pointer is null\n");
            return false;
        }

        let game_ref = unsafe { &*game };
        let path = unsafe { c_string(game_ref.path) };
        log_global(
            ffi::RETRO_LOG_INFO,
            format!("retro_load_game: path={:?}, data={:p}, size={}\n", path, game_ref.data, game_ref.size).as_bytes(),
        );

        let content = unsafe { content_from_game_info(game_ref) };
        match content {
            Ok(content) => {
                log_global(
                    ffi::RETRO_LOG_INFO,
                    format!(
                        "retro_load_game: content loaded, name={}, data_len={}\n",
                        content.name,
                        content.data.len()
                    )
                    .as_bytes(),
                );
                if let Ok(mut state) = STATE.lock() {
                    load_locked(&mut state, content)
                } else {
                    false
                }
            }
            Err(message) => {
                log_global(
                    ffi::RETRO_LOG_ERROR,
                    format!("retro_load_game: failed to parse content: {message}\n").as_bytes(),
                );
                false
            }
        }
    })
}

#[unsafe(no_mangle)]
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn retro_load_game_special(_game_type: c_uint, _info: *const RetroGameInfo, _num_info: usize) -> bool {
    ffi_guard_bool(|| false)
}

#[unsafe(no_mangle)]
pub extern "C" fn retro_unload_game() {
    ffi_guard(|| {
        if let Ok(mut state) = STATE.lock() {
            unload_locked(&mut state);
        }
    });
}

#[unsafe(no_mangle)]
pub extern "C" fn retro_run() {
    ffi_guard(|| {
        if let Ok(mut state) = STATE.lock() {
            let callbacks = RunCallbacks {
                environ: state.environ,
                video_refresh: state.video_refresh,
                audio_sample: state.audio_sample,
                audio_sample_batch: state.audio_sample_batch,
                input_poll: state.input_poll,
                input_state: state.input_state,
                log: state.log,
            };
            if let Some(core) = state.core.as_mut() {
                core.run(callbacks);
            }
        }
    });
}

#[unsafe(no_mangle)]
pub extern "C" fn retro_serialize_size() -> usize {
    0
}

#[unsafe(no_mangle)]
pub extern "C" fn retro_serialize(_data: *mut c_void, _size: usize) -> bool {
    false
}

#[unsafe(no_mangle)]
pub extern "C" fn retro_unserialize(_data: *const c_void, _size: usize) -> bool {
    false
}

#[unsafe(no_mangle)]
pub extern "C" fn retro_cheat_reset() {}

#[unsafe(no_mangle)]
pub extern "C" fn retro_cheat_set(_index: c_uint, _enabled: bool, _code: *const std::ffi::c_char) {}

#[unsafe(no_mangle)]
pub extern "C" fn retro_get_region() -> c_uint {
    RETRO_REGION_NTSC
}

#[unsafe(no_mangle)]
pub extern "C" fn retro_get_memory_data(id: c_uint) -> *mut c_void {
    match id {
        RETRO_MEMORY_SAVE_RAM | RETRO_MEMORY_RTC | RETRO_MEMORY_SYSTEM_RAM | RETRO_MEMORY_VIDEO_RAM => ptr::null_mut(),
        _ => ptr::null_mut(),
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn retro_get_memory_size(_id: c_uint) -> usize {
    0
}

fn load_locked(state: &mut GlobalState, content: LoadedContent) -> bool {
    unload_locked(state);

    let options = read_core_options(state.environ);
    let callbacks = CoreStartCallbacks {
        environ: state.environ,
        log: state.log,
    };

    match LibretroCore::load(content, options, state.save_dir.clone(), callbacks) {
        Ok(core) => {
            state.log.write(ffi::RETRO_LOG_INFO, b"load_locked: emulator loaded successfully\n");
            state.core = Some(core);
            true
        }
        Err(err) => {
            state
                .log
                .write(ffi::RETRO_LOG_ERROR, format!("load_locked: failed to load emulator: {err}\n").as_bytes());
            false
        }
    }
}

fn unload_locked(state: &mut GlobalState) {
    if let Some(mut core) = state.core.take() {
        core.shutdown();
    }
}

unsafe fn content_from_game_info(game: &RetroGameInfo) -> std::result::Result<LoadedContent, String> {
    if let Some(ext) = game_info_ext() {
        let ext = unsafe { &*ext };
        let path = unsafe { c_string(ext.full_path) }.map(PathBuf::from);
        let name = content_name(
            path.as_deref(),
            unsafe { c_string(ext.name) }.as_deref(),
            unsafe { c_string(ext.ext) }.as_deref(),
        );
        let data = if !ext.data.is_null() && ext.size > 0 {
            unsafe { slice::from_raw_parts(ext.data.cast::<u8>(), ext.size) }.to_vec()
        } else if let Some(path) = &path {
            std::fs::read(path).map_err(|err| format!("Failed to read content {path:?}: {err}"))?
        } else {
            return Err("Frontend did not provide content data or a readable full path".to_owned());
        };

        return Ok(LoadedContent::single(name, data));
    }

    let part = unsafe { content_part_from_game_info(game)? };
    Ok(LoadedContent::single(part.name, part.data))
}

struct ContentPart {
    name: String,
    data: Vec<u8>,
}

unsafe fn content_part_from_game_info(game: &RetroGameInfo) -> std::result::Result<ContentPart, String> {
    let path = unsafe { c_string(game.path) }.map(PathBuf::from);
    let name = path
        .as_deref()
        .and_then(|path| path.file_name())
        .and_then(|name| name.to_str())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| "content".to_owned());
    let data = if !game.data.is_null() && game.size > 0 {
        unsafe { slice::from_raw_parts(game.data.cast::<u8>(), game.size) }.to_vec()
    } else if let Some(path) = &path {
        std::fs::read(path).map_err(|err| format!("Failed to read content {path:?}: {err}"))?
    } else {
        return Err("Frontend did not provide content data or a readable path".to_owned());
    };

    Ok(ContentPart { name, data })
}

fn game_info_ext() -> Option<*const RetroGameInfoExt> {
    let state = STATE.lock().ok()?;
    let environ = state.environ?;
    drop(state);

    let mut ext: *const RetroGameInfoExt = ptr::null();
    let ok = unsafe {
        environ(
            ffi::RETRO_ENVIRONMENT_GET_GAME_INFO_EXT,
            (&mut ext as *mut *const RetroGameInfoExt).cast::<c_void>(),
        )
    };
    (ok && !ext.is_null()).then_some(ext)
}

fn content_name(path: Option<&Path>, name: Option<&str>, extension: Option<&str>) -> String {
    if let Some(path) = path
        && let Some(file_name) = path.file_name().and_then(|file_name| file_name.to_str())
    {
        return file_name.to_owned();
    }

    match (name, extension) {
        (Some(name), Some(extension)) if !extension.is_empty() => format!("{name}.{extension}"),
        (Some(name), _) => name.to_owned(),
        _ => "content".to_owned(),
    }
}

fn get_log_interface(environ: Option<RetroEnvironmentT>) -> LogInterface {
    let Some(environ) = environ else {
        return LogInterface { log: None };
    };

    let mut callback = RetroLogCallback { log: None };
    let ok = unsafe {
        environ(
            RETRO_ENVIRONMENT_GET_LOG_INTERFACE,
            (&mut callback as *mut RetroLogCallback).cast::<c_void>(),
        )
    };

    LogInterface {
        log: ok.then_some(callback.log).flatten(),
    }
}

fn av_info(options: &CoreOptions) -> RetroSystemAvInfo {
    RetroSystemAvInfo {
        geometry: RetroGameGeometry {
            base_width: options.width,
            base_height: options.height,
            max_width: options.width,
            max_height: options.height,
            aspect_ratio: options.width as f32 / options.height as f32,
        },
        timing: RetroSystemTiming {
            fps: audio::AUDIO_FPS,
            sample_rate: audio::AUDIO_SAMPLE_RATE as f64,
        },
    }
}

unsafe fn c_string(ptr: *const std::ffi::c_char) -> Option<String> {
    if ptr.is_null() {
        return None;
    }

    unsafe { CStr::from_ptr(ptr) }.to_str().ok().map(ToOwned::to_owned)
}

fn log_global(level: u32, message: &[u8]) {
    if let Ok(state) = STATE.lock() {
        state.log.write(level, message);
    }
}

fn ffi_guard(f: impl FnOnce()) {
    let _ = catch_unwind(AssertUnwindSafe(f));
}

fn ffi_guard_bool(f: impl FnOnce() -> bool) -> bool {
    catch_unwind(AssertUnwindSafe(f)).unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use std::{
        ffi::{CString, c_char, c_uint, c_void},
        ptr,
        sync::{
            Mutex, OnceLock,
            atomic::{AtomicUsize, Ordering},
        },
        thread,
        time::Duration,
    };

    use crate::{
        ffi::{
            RETRO_ENVIRONMENT_GET_CORE_OPTIONS_VERSION, RETRO_ENVIRONMENT_GET_SAVE_DIRECTORY, RETRO_ENVIRONMENT_GET_VARIABLE,
            RETRO_ENVIRONMENT_GET_VARIABLE_UPDATE, RETRO_ENVIRONMENT_SET_CORE_OPTIONS, RETRO_ENVIRONMENT_SET_CORE_OPTIONS_V2,
            RETRO_ENVIRONMENT_SET_FASTFORWARDING_OVERRIDE, RETRO_ENVIRONMENT_SET_INPUT_DESCRIPTORS, RETRO_ENVIRONMENT_SET_PIXEL_FORMAT,
            RetroGameInfo, RetroVariable,
        },
        retro_deinit, retro_init, retro_load_game, retro_run, retro_set_audio_sample_batch, retro_set_environment, retro_set_input_poll,
        retro_set_input_state, retro_set_video_refresh, retro_unload_game,
    };

    static TEST_LOCK: Mutex<()> = Mutex::new(());
    static SAVE_DIR: OnceLock<CString> = OnceLock::new();
    static VIDEO_FRAMES: AtomicUsize = AtomicUsize::new(0);

    #[test]
    fn libretro_lifecycle_loads_ktf_helloworld_and_presents_frames() {
        let _guard = TEST_LOCK.lock().unwrap();
        VIDEO_FRAMES.store(0, Ordering::SeqCst);

        let temp_dir = std::env::temp_dir().join("wie-libretro-smoke");
        std::fs::create_dir_all(&temp_dir).unwrap();
        let _ = SAVE_DIR.set(CString::new(temp_dir.to_string_lossy().as_bytes()).unwrap());

        retro_set_environment(test_environment);
        retro_set_video_refresh(test_video_refresh);
        retro_set_audio_sample_batch(test_audio_sample_batch);
        retro_set_input_poll(test_input_poll);
        retro_set_input_state(test_input_state);
        retro_init();

        let data = std::fs::read("../test_data/helloworld_ktf.zip").unwrap();
        let path = CString::new("helloworld_ktf.zip").unwrap();
        let game = RetroGameInfo {
            path: path.as_ptr(),
            data: data.as_ptr().cast::<c_void>(),
            size: data.len(),
            meta: ptr::null(),
        };
        assert!(retro_load_game(&game));

        for _ in 0..180 {
            retro_run();
            if VIDEO_FRAMES.load(Ordering::SeqCst) > 0 {
                break;
            }
            thread::sleep(Duration::from_millis(16));
        }

        retro_unload_game();
        retro_deinit();

        assert!(VIDEO_FRAMES.load(Ordering::SeqCst) > 0);
    }

    unsafe extern "C" fn test_environment(cmd: c_uint, data: *mut c_void) -> bool {
        match cmd {
            RETRO_ENVIRONMENT_GET_CORE_OPTIONS_VERSION => {
                unsafe {
                    *data.cast::<c_uint>() = 2;
                }
                true
            }
            RETRO_ENVIRONMENT_GET_SAVE_DIRECTORY => {
                unsafe {
                    *data.cast::<*const c_char>() = SAVE_DIR.get().unwrap().as_ptr();
                }
                true
            }
            RETRO_ENVIRONMENT_GET_VARIABLE => {
                unsafe {
                    (*data.cast::<RetroVariable>()).value = ptr::null();
                }
                true
            }
            RETRO_ENVIRONMENT_GET_VARIABLE_UPDATE => {
                unsafe {
                    *data.cast::<bool>() = false;
                }
                true
            }
            RETRO_ENVIRONMENT_SET_PIXEL_FORMAT
            | RETRO_ENVIRONMENT_SET_INPUT_DESCRIPTORS
            | RETRO_ENVIRONMENT_SET_CORE_OPTIONS
            | RETRO_ENVIRONMENT_SET_CORE_OPTIONS_V2
            | RETRO_ENVIRONMENT_SET_FASTFORWARDING_OVERRIDE => true,
            _ => false,
        }
    }

    unsafe extern "C" fn test_video_refresh(data: *const c_void, width: c_uint, height: c_uint, _pitch: usize) {
        VIDEO_FRAMES.fetch_add(1, Ordering::SeqCst);
        if data.is_null() || width == 0 || height == 0 {
            return;
        }

        let _ = unsafe { std::slice::from_raw_parts(data.cast::<u32>(), (width * height) as usize) };
    }

    unsafe extern "C" fn test_audio_sample_batch(_data: *const i16, frames: usize) -> usize {
        frames
    }

    unsafe extern "C" fn test_input_poll() {}

    unsafe extern "C" fn test_input_state(_port: c_uint, _device: c_uint, _index: c_uint, _id: c_uint) -> i16 {
        0
    }
}
