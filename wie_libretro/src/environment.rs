use std::{
    ffi::{CStr, CString, c_char, c_uint, c_void},
    path::{Path, PathBuf},
    ptr,
};

use crate::ffi::{
    RETRO_ENVIRONMENT_GET_CORE_OPTIONS_VERSION, RETRO_ENVIRONMENT_GET_LANGUAGE, RETRO_ENVIRONMENT_GET_SAVE_DIRECTORY,
    RETRO_ENVIRONMENT_GET_SYSTEM_DIRECTORY, RETRO_ENVIRONMENT_GET_VARIABLE, RETRO_ENVIRONMENT_GET_VARIABLE_UPDATE,
    RETRO_ENVIRONMENT_SET_CORE_OPTIONS, RETRO_ENVIRONMENT_SET_CORE_OPTIONS_INTL, RETRO_ENVIRONMENT_SET_CORE_OPTIONS_V2,
    RETRO_ENVIRONMENT_SET_CORE_OPTIONS_V2_INTL, RETRO_ENVIRONMENT_SET_FASTFORWARDING_OVERRIDE, RETRO_ENVIRONMENT_SET_VARIABLES,
    RETRO_LANGUAGE_CHINESE_SIMPLIFIED, RETRO_LANGUAGE_CHINESE_TRADITIONAL, RetroCoreOptionDefinition, RetroCoreOptionV2Category,
    RetroCoreOptionV2Definition, RetroCoreOptionValue, RetroCoreOptionsIntl, RetroCoreOptionsV2, RetroCoreOptionsV2Intl, RetroEnvironmentT,
    RetroFastforwardingOverride, RetroVariable,
};

const C_EMPTY: &[u8] = b"\0";
const C_VIDEO: &[u8] = b"video\0";
const C_AUDIO: &[u8] = b"audio\0";
const C_SYSTEM: &[u8] = b"system\0";

const C_WIE_RESOLUTION: &[u8] = b"wie_resolution\0";
const C_WIE_RUNTIME: &[u8] = b"wie_runtime\0";
const C_WIE_MIDI: &[u8] = b"wie_midi\0";
const C_WIE_MIDI_VOLUME: &[u8] = b"wie_midi_volume\0";
const C_WIE_MIDI_SOUNDFONT: &[u8] = b"wie_midi_soundfont\0";

const C_128X128: &[u8] = b"128x128\0";
const C_128X160: &[u8] = b"128x160\0";
const C_176X208: &[u8] = b"176x208\0";
const C_240X320: &[u8] = b"240x320\0";
const C_AUTO: &[u8] = b"auto\0";
const C_KTF: &[u8] = b"ktf\0";
const C_LGT: &[u8] = b"lgt\0";
const C_SKT: &[u8] = b"skt\0";
const C_J2ME: &[u8] = b"j2me\0";
const C_ON: &[u8] = b"on\0";
const C_OFF: &[u8] = b"off\0";
const C_BUILTIN: &[u8] = b"builtin\0";
const C_5: &[u8] = b"5\0";
const MIDI_VOLUME_VALUES: &[(&[u8], Option<&str>)] = &[
    (b"0\0", None),
    (b"1\0", None),
    (b"2\0", None),
    (b"3\0", None),
    (b"4\0", None),
    (C_5, None),
    (b"6\0", None),
    (b"7\0", None),
    (b"8\0", None),
    (b"9\0", None),
    (b"10\0", None),
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuntimeOption {
    Auto,
    Ktf,
    Lgt,
    Skt,
    J2me,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CoreOptions {
    pub width: u32,
    pub height: u32,
    pub runtime: RuntimeOption,
    pub midi_enabled: bool,
    pub midi_volume: u8,
    pub sound_font_path: Option<PathBuf>,
}

impl Default for CoreOptions {
    fn default() -> Self {
        Self {
            width: 240,
            height: 320,
            runtime: RuntimeOption::Auto,
            midi_enabled: true,
            midi_volume: 5,
            sound_font_path: None,
        }
    }
}

pub fn register_environment(environ: RetroEnvironmentT) {
    disable_fastforward(environ);
    register_core_options(environ);
}

fn disable_fastforward(environ: RetroEnvironmentT) {
    let mut ff_override = RetroFastforwardingOverride {
        ratio: -1.0,
        fastforward: false,
        notification: false,
        inhibit_toggle: true,
    };
    unsafe {
        environ(
            RETRO_ENVIRONMENT_SET_FASTFORWARDING_OVERRIDE,
            (&mut ff_override as *mut RetroFastforwardingOverride).cast::<c_void>(),
        );
    }
}

pub fn get_directories(environ: Option<RetroEnvironmentT>) -> PathBuf {
    query_directory(environ, RETRO_ENVIRONMENT_GET_SAVE_DIRECTORY)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| std::env::temp_dir()))
        .join("wie")
}

fn sound_font_directory(environ: Option<RetroEnvironmentT>) -> PathBuf {
    query_directory(environ, RETRO_ENVIRONMENT_GET_SYSTEM_DIRECTORY)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| std::env::temp_dir()))
        .join("wie")
        .join("sf2")
}

pub fn read_core_options(environ: Option<RetroEnvironmentT>) -> CoreOptions {
    let mut options = CoreOptions::default();
    let Some(environ) = environ else {
        return options;
    };

    match get_variable(environ, C_WIE_RESOLUTION) {
        Some(value) if value == "128x128" => {
            options.width = 128;
            options.height = 128;
        }
        Some(value) if value == "128x160" => {
            options.width = 128;
            options.height = 160;
        }
        Some(value) if value == "176x208" => {
            options.width = 176;
            options.height = 208;
        }
        _ => {}
    }

    options.runtime = match get_variable(environ, C_WIE_RUNTIME).as_deref() {
        Some("ktf") => RuntimeOption::Ktf,
        Some("lgt") => RuntimeOption::Lgt,
        Some("skt") => RuntimeOption::Skt,
        Some("j2me") => RuntimeOption::J2me,
        _ => RuntimeOption::Auto,
    };
    options.midi_enabled = !matches!(get_variable(environ, C_WIE_MIDI).as_deref(), Some("off"));
    if let Some(volume) = get_variable(environ, C_WIE_MIDI_VOLUME)
        .and_then(|value| value.parse::<u8>().ok())
        .filter(|volume| *volume <= 10)
    {
        options.midi_volume = volume;
    }
    if let Some(value) = get_variable(environ, C_WIE_MIDI_SOUNDFONT)
        && value != "builtin"
        && !value.contains(['/', '\\', '\0'])
    {
        let path = sound_font_directory(Some(environ)).join(value);
        if path.is_file() {
            options.sound_font_path = Some(path);
        }
    }

    options
}

pub fn core_options_updated(environ: Option<RetroEnvironmentT>) -> bool {
    let Some(environ) = environ else {
        return false;
    };

    let mut updated = false;
    (unsafe { environ(RETRO_ENVIRONMENT_GET_VARIABLE_UPDATE, (&mut updated as *mut bool).cast::<c_void>()) }) && updated
}

fn register_core_options(environ: RetroEnvironmentT) {
    let mut version = 0 as c_uint;
    let has_version = unsafe { environ(RETRO_ENVIRONMENT_GET_CORE_OPTIONS_VERSION, (&mut version as *mut c_uint).cast::<c_void>()) };
    let locale = frontend_locale(environ);
    let sound_fonts = sound_font_options(sound_font_directory(Some(environ)));

    if has_version && version >= 2 {
        let us = core_options_v2(Locale::English, &sound_fonts);
        if locale != Locale::English {
            let local = core_options_v2(locale, &sound_fonts);
            let intl = Box::leak(Box::new(RetroCoreOptionsV2Intl { us, local }));
            if unsafe {
                environ(
                    RETRO_ENVIRONMENT_SET_CORE_OPTIONS_V2_INTL,
                    (intl as *mut RetroCoreOptionsV2Intl).cast::<c_void>(),
                )
            } {
                return;
            }
        }

        unsafe { environ(RETRO_ENVIRONMENT_SET_CORE_OPTIONS_V2, us.cast::<c_void>()) };
    } else if has_version && version >= 1 {
        let us = core_options_v1(Locale::English, &sound_fonts);
        if locale != Locale::English {
            let local = core_options_v1(locale, &sound_fonts);
            let intl = Box::leak(Box::new(RetroCoreOptionsIntl { us, local }));
            if unsafe {
                environ(
                    RETRO_ENVIRONMENT_SET_CORE_OPTIONS_INTL,
                    (intl as *mut RetroCoreOptionsIntl).cast::<c_void>(),
                )
            } {
                return;
            }
        }

        unsafe { environ(RETRO_ENVIRONMENT_SET_CORE_OPTIONS, us.cast::<c_void>()) };
    } else {
        let variables = vec![
            variable(
                C_WIE_RESOLUTION,
                text(
                    locale,
                    "Screen Resolution; 240x320|176x208|128x160|128x128\0",
                    "屏幕分辨率; 240x320|176x208|128x160|128x128\0",
                    "螢幕解析度; 240x320|176x208|128x160|128x128\0",
                ),
            ),
            variable(
                C_WIE_RUNTIME,
                text(
                    locale,
                    "Runtime; auto|ktf|lgt|skt|j2me\0",
                    "运行平台; auto|ktf|lgt|skt|j2me\0",
                    "執行平台; auto|ktf|lgt|skt|j2me\0",
                ),
            ),
            variable(
                C_WIE_MIDI,
                text(locale, "MIDI Music; on|off\0", "MIDI 音乐; on|off\0", "MIDI 音樂; on|off\0"),
            ),
            variable(
                C_WIE_MIDI_VOLUME,
                text(
                    locale,
                    "MIDI Synth Volume; 5|0|1|2|3|4|6|7|8|9|10\0",
                    "MIDI 合成音量; 5|0|1|2|3|4|6|7|8|9|10\0",
                    "MIDI 合成音量; 5|0|1|2|3|4|6|7|8|9|10\0",
                ),
            ),
            variable_dynamic(C_WIE_MIDI_SOUNDFONT, legacy_sound_font_variable(locale, &sound_fonts)),
            RetroVariable {
                key: ptr::null(),
                value: ptr::null(),
            },
        ];
        let variables = Box::leak(variables.into_boxed_slice());
        unsafe {
            environ(RETRO_ENVIRONMENT_SET_VARIABLES, variables.as_mut_ptr().cast::<c_void>());
        }
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum Locale {
    English,
    ZhHans,
    ZhHant,
}

fn frontend_locale(environ: RetroEnvironmentT) -> Locale {
    let mut language = 0 as c_uint;
    if !unsafe { environ(RETRO_ENVIRONMENT_GET_LANGUAGE, (&mut language as *mut c_uint).cast::<c_void>()) } {
        return Locale::English;
    }

    match language {
        RETRO_LANGUAGE_CHINESE_SIMPLIFIED => Locale::ZhHans,
        RETRO_LANGUAGE_CHINESE_TRADITIONAL => Locale::ZhHant,
        _ => Locale::English,
    }
}

fn core_options_v2(locale: Locale, sound_fonts: &[String]) -> *mut RetroCoreOptionsV2 {
    let categories = Box::leak(v2_categories(locale).into_boxed_slice());
    let definitions = Box::leak(v2_definitions(locale, sound_fonts).into_boxed_slice());
    Box::leak(Box::new(RetroCoreOptionsV2 {
        categories: categories.as_mut_ptr(),
        definitions: definitions.as_mut_ptr(),
    }))
}

fn v2_categories(locale: Locale) -> Vec<RetroCoreOptionV2Category> {
    vec![
        category(
            C_VIDEO,
            text(locale, "Video\0", "视频\0", "影像\0"),
            text(locale, "Screen size.\0", "屏幕尺寸。\0", "螢幕尺寸。\0"),
        ),
        category(
            C_AUDIO,
            text(locale, "Audio\0", "音频\0", "音訊\0"),
            text(locale, "Music settings.\0", "音乐设置。\0", "音樂設定。\0"),
        ),
        category(
            C_SYSTEM,
            text(locale, "System\0", "系统\0", "系統\0"),
            text(locale, "Content runtime selection.\0", "内容运行平台选择。\0", "內容執行平台選擇。\0"),
        ),
        RetroCoreOptionV2Category {
            key: ptr::null(),
            desc: ptr::null(),
            info: ptr::null(),
        },
    ]
}

fn v2_definitions(locale: Locale, sound_fonts: &[String]) -> Vec<RetroCoreOptionV2Definition> {
    let on_off = &[
        (C_ON, Some(text(locale, "On\0", "开启\0", "開啟\0"))),
        (C_OFF, Some(text(locale, "Off\0", "关闭\0", "關閉\0"))),
    ];
    vec![
        v2_option(
            C_WIE_RESOLUTION,
            text(locale, "Screen Resolution\0", "屏幕分辨率\0", "螢幕解析度\0"),
            text(locale, "Resolution\0", "分辨率\0", "解析度\0"),
            text(
                locale,
                "Screen size exposed to the emulated app. Requires reload.\0",
                "程式的屏幕尺寸。需重载核心。\0",
                "程式的螢幕尺寸。需重載核心。\0",
            ),
            C_VIDEO,
            C_240X320,
            OptionValues::Static(&[(C_240X320, None), (C_176X208, None), (C_128X160, None), (C_128X128, None)]),
        ),
        v2_option(
            C_WIE_RUNTIME,
            text(locale, "Runtime\0", "运行平台\0", "執行平台\0"),
            text(locale, "Runtime\0", "平台\0", "平台\0"),
            text(
                locale,
                "Force a platform runtime, or leave content detection automatic.\0",
                "强制指定平台运行时，默认自动识别。\0",
                "強制指定平台執行環境，預設自動辨識。\0",
            ),
            C_SYSTEM,
            C_AUTO,
            OptionValues::Static(&[
                (C_AUTO, Some(text(locale, "Auto\0", "自动\0", "自動\0"))),
                (C_KTF, None),
                (C_LGT, None),
                (C_SKT, None),
                (C_J2ME, None),
            ]),
        ),
        v2_option(
            C_WIE_MIDI,
            text(locale, "MIDI Music\0", "MIDI 音乐\0", "MIDI 音樂\0"),
            text(locale, "MIDI\0", "MIDI\0", "MIDI\0"),
            text(
                locale,
                "Enable synthesized MIDI music.\0",
                "启用软件合成的 MIDI 音乐。\0",
                "啟用軟體合成的 MIDI 音樂。\0",
            ),
            C_AUDIO,
            C_ON,
            OptionValues::Static(on_off),
        ),
        v2_option(
            C_WIE_MIDI_SOUNDFONT,
            text(locale, "MIDI SoundFont\0", "MIDI 音色库\0", "MIDI 音色庫\0"),
            text(locale, "SoundFont\0", "音色库\0", "音色庫\0"),
            text(
                locale,
                "SF2 file from system/wie/sf2. Requires reload.\0",
                "来自 system/wie/sf2 的 SF2 文件。需重载核心。\0",
                "來自 system/wie/sf2 的 SF2 檔案。需重載核心。\0",
            ),
            C_AUDIO,
            C_BUILTIN,
            OptionValues::Dynamic(sound_font_values(locale, sound_fonts)),
        ),
        v2_option(
            C_WIE_MIDI_VOLUME,
            text(locale, "MIDI Synth Volume\0", "MIDI 合成音量\0", "MIDI 合成音量\0"),
            text(locale, "MIDI Volume\0", "MIDI 音量\0", "MIDI 音量\0"),
            text(
                locale,
                "MIDI synthesizer volume. Requires reload.\0",
                "MIDI 合成器音量。需重载核心。\0",
                "MIDI 合成器音量。需重載核心。\0",
            ),
            C_AUDIO,
            C_5,
            OptionValues::Static(MIDI_VOLUME_VALUES),
        ),
        RetroCoreOptionV2Definition {
            key: ptr::null(),
            desc: ptr::null(),
            desc_categorized: ptr::null(),
            info: ptr::null(),
            info_categorized: ptr::null(),
            category_key: ptr::null(),
            values: [RetroCoreOptionValue::empty(); crate::ffi::RETRO_NUM_CORE_OPTION_VALUES_MAX],
            default_value: ptr::null(),
        },
    ]
}

fn core_options_v1(locale: Locale, sound_fonts: &[String]) -> *mut RetroCoreOptionDefinition {
    let definitions = Box::leak(v1_definitions(locale, sound_fonts).into_boxed_slice());
    definitions.as_mut_ptr()
}

fn v1_definitions(locale: Locale, sound_fonts: &[String]) -> Vec<RetroCoreOptionDefinition> {
    let on_off = &[
        (C_ON, Some(text(locale, "On\0", "开启\0", "開啟\0"))),
        (C_OFF, Some(text(locale, "Off\0", "关闭\0", "關閉\0"))),
    ];
    vec![
        v1_option(
            C_WIE_RESOLUTION,
            text(locale, "Screen Resolution\0", "屏幕分辨率\0", "螢幕解析度\0"),
            text(
                locale,
                "Screen size exposed to the emulated app. Requires reload.\0",
                "程序的屏幕尺寸。需重载核心。\0",
                "程式的螢幕尺寸。需重載核心。\0",
            ),
            C_240X320,
            OptionValues::Static(&[(C_240X320, None), (C_176X208, None), (C_128X160, None), (C_128X128, None)]),
        ),
        v1_option(
            C_WIE_RUNTIME,
            text(locale, "Runtime\0", "运行平台\0", "執行平台\0"),
            text(
                locale,
                "Force a platform runtime, or leave content detection automatic.\0",
                "强制指定平台运行时，默认自动识别。\0",
                "強制指定平台執行環境，預設自動辨識。\0",
            ),
            C_AUTO,
            OptionValues::Static(&[
                (C_AUTO, Some(text(locale, "Auto\0", "自动\0", "自動\0"))),
                (C_KTF, None),
                (C_LGT, None),
                (C_SKT, None),
                (C_J2ME, None),
            ]),
        ),
        v1_option(
            C_WIE_MIDI,
            text(locale, "MIDI Music\0", "MIDI 音乐\0", "MIDI 音樂\0"),
            text(
                locale,
                "Enable synthesized MIDI music.\0",
                "启用软件合成的 MIDI 音乐。\0",
                "啟用軟體合成的 MIDI 音樂。\0",
            ),
            C_ON,
            OptionValues::Static(on_off),
        ),
        v1_option(
            C_WIE_MIDI_SOUNDFONT,
            text(locale, "MIDI SoundFont\0", "MIDI 音色库\0", "MIDI 音色庫\0"),
            text(
                locale,
                "SF2 file from system/wie/sf2. Requires reload.\0",
                "来自 system/wie/sf2 的 SF2 文件。需重载核心。\0",
                "來自 system/wie/sf2 的 SF2 檔案。需重載核心。\0",
            ),
            C_BUILTIN,
            OptionValues::Dynamic(sound_font_values(locale, sound_fonts)),
        ),
        v1_option(
            C_WIE_MIDI_VOLUME,
            text(locale, "MIDI Synth Volume\0", "MIDI 合成音量\0", "MIDI 合成音量\0"),
            text(
                locale,
                "MIDI synthesizer volume. Requires reload.\0",
                "MIDI 合成器音量。需重载核心。\0",
                "MIDI 合成器音量。需重載核心。\0",
            ),
            C_5,
            OptionValues::Static(MIDI_VOLUME_VALUES),
        ),
        RetroCoreOptionDefinition {
            key: ptr::null(),
            desc: ptr::null(),
            info: ptr::null(),
            values: [RetroCoreOptionValue::empty(); crate::ffi::RETRO_NUM_CORE_OPTION_VALUES_MAX],
            default_value: ptr::null(),
        },
    ]
}

fn text(locale: Locale, en: &'static str, zh_hans: &'static str, zh_hant: &'static str) -> &'static str {
    match locale {
        Locale::English => en,
        Locale::ZhHans => zh_hans,
        Locale::ZhHant => zh_hant,
    }
}

fn query_directory(environ: Option<RetroEnvironmentT>, command: c_uint) -> Option<PathBuf> {
    let environ = environ?;
    let mut path_ptr: *const c_char = ptr::null();
    let ok = unsafe { environ(command, (&mut path_ptr as *mut *const c_char).cast::<c_void>()) };
    if !ok || path_ptr.is_null() {
        return None;
    }

    let path = unsafe { CStr::from_ptr(path_ptr) }.to_str().ok()?;
    if path.is_empty() {
        return None;
    }

    Some(PathBuf::from(path))
}

fn get_variable(environ: RetroEnvironmentT, key: &'static [u8]) -> Option<String> {
    let mut variable = RetroVariable {
        key: c_ptr(key),
        value: ptr::null(),
    };
    let ok = unsafe { environ(RETRO_ENVIRONMENT_GET_VARIABLE, (&mut variable as *mut RetroVariable).cast::<c_void>()) };
    if !ok || variable.value.is_null() {
        return None;
    }

    unsafe { CStr::from_ptr(variable.value) }.to_str().ok().map(ToOwned::to_owned)
}

fn category(key: &'static [u8], desc: &'static str, info: &'static str) -> RetroCoreOptionV2Category {
    RetroCoreOptionV2Category {
        key: c_ptr(key),
        desc: c_str(desc),
        info: c_str(info),
    }
}

fn variable(key: &'static [u8], value: &'static str) -> RetroVariable {
    RetroVariable {
        key: c_ptr(key),
        value: c_str(value),
    }
}

fn variable_dynamic(key: &'static [u8], value: *const c_char) -> RetroVariable {
    RetroVariable { key: c_ptr(key), value }
}

fn v1_option(
    key: &'static [u8],
    desc: &'static str,
    info: &'static str,
    default_value: &'static [u8],
    values: OptionValues<'_>,
) -> RetroCoreOptionDefinition {
    RetroCoreOptionDefinition {
        key: c_ptr(key),
        desc: c_str(desc),
        info: c_str(info),
        values: values.into_array(),
        default_value: c_ptr(default_value),
    }
}

fn v2_option(
    key: &'static [u8],
    desc: &'static str,
    desc_categorized: &'static str,
    info: &'static str,
    category_key: &'static [u8],
    default_value: &'static [u8],
    values: OptionValues<'_>,
) -> RetroCoreOptionV2Definition {
    RetroCoreOptionV2Definition {
        key: c_ptr(key),
        desc: c_str(desc),
        desc_categorized: c_str(desc_categorized),
        info: c_str(info),
        info_categorized: c_ptr(C_EMPTY),
        category_key: c_ptr(category_key),
        values: values.into_array(),
        default_value: c_ptr(default_value),
    }
}

enum OptionValues<'a> {
    Static(&'a [(&'static [u8], Option<&'static str>)]),
    Dynamic(Vec<RetroCoreOptionValue>),
}

impl OptionValues<'_> {
    fn into_array(self) -> [RetroCoreOptionValue; crate::ffi::RETRO_NUM_CORE_OPTION_VALUES_MAX] {
        let mut out = [RetroCoreOptionValue::empty(); crate::ffi::RETRO_NUM_CORE_OPTION_VALUES_MAX];
        match self {
            OptionValues::Static(values) => {
                for (index, (value, label)) in values.iter().enumerate() {
                    out[index] = RetroCoreOptionValue {
                        value: c_ptr(value),
                        label: label.map(c_str).unwrap_or(ptr::null()),
                    };
                }
            }
            OptionValues::Dynamic(values) => {
                for (index, value) in values.into_iter().take(crate::ffi::RETRO_NUM_CORE_OPTION_VALUES_MAX - 1).enumerate() {
                    out[index] = value;
                }
            }
        }
        out
    }
}

fn sound_font_options(directory: PathBuf) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return Vec::new();
    };

    let mut options = entries
        .filter_map(|entry| {
            let path = entry.ok()?.path();
            if !path.is_file() || !is_sf2_path(&path) {
                return None;
            }
            let file_name = path.file_name()?.to_str()?.to_owned();
            (!file_name.contains('\0')).then_some(file_name)
        })
        .collect::<Vec<_>>();
    options.sort_by_key(|file_name| file_name.to_ascii_lowercase());
    options.truncate(crate::ffi::RETRO_NUM_CORE_OPTION_VALUES_MAX - 2);
    options
}

fn is_sf2_path(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("sf2"))
}

fn sound_font_values(locale: Locale, sound_fonts: &[String]) -> Vec<RetroCoreOptionValue> {
    let mut values = vec![RetroCoreOptionValue {
        value: c_ptr(C_BUILTIN),
        label: c_str(text(locale, "Built-in\0", "内置\0", "內建\0")),
    }];
    values.extend(sound_fonts.iter().map(|file_name| RetroCoreOptionValue {
        value: leak_c_string(file_name),
        label: ptr::null(),
    }));
    values
}

fn legacy_sound_font_variable(locale: Locale, sound_fonts: &[String]) -> *const c_char {
    let mut text = match locale {
        Locale::English => "MIDI SoundFont; builtin".to_owned(),
        Locale::ZhHans => "MIDI 音色库; builtin".to_owned(),
        Locale::ZhHant => "MIDI 音色庫; builtin".to_owned(),
    };
    for file_name in sound_fonts {
        text.push('|');
        text.push_str(file_name);
    }
    leak_c_string(text)
}

fn leak_c_string(text: impl AsRef<str>) -> *const c_char {
    CString::new(text.as_ref())
        .map(|text| text.into_raw().cast_const())
        .unwrap_or(ptr::null())
}

fn c_ptr(bytes: &'static [u8]) -> *const c_char {
    bytes.as_ptr().cast::<c_char>()
}

fn c_str(text: &'static str) -> *const c_char {
    text.as_ptr().cast::<c_char>()
}

#[cfg(test)]
mod tests {
    use std::{
        ffi::{c_uint, c_void},
        sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering},
    };

    use crate::ffi::{RETRO_ENVIRONMENT_SET_FASTFORWARDING_OVERRIDE, RetroFastforwardingOverride};

    use super::{register_environment, sound_font_options};

    static FASTFORWARD_OVERRIDE_CALLS: AtomicUsize = AtomicUsize::new(0);
    static FASTFORWARD_RATIO_BITS: AtomicU32 = AtomicU32::new(0);
    static FASTFORWARD_FLAG: AtomicBool = AtomicBool::new(true);
    static FASTFORWARD_NOTIFICATION: AtomicBool = AtomicBool::new(true);
    static FASTFORWARD_INHIBIT_TOGGLE: AtomicBool = AtomicBool::new(false);

    #[test]
    fn register_environment_disables_fastforward_toggle() {
        FASTFORWARD_OVERRIDE_CALLS.store(0, Ordering::SeqCst);
        FASTFORWARD_RATIO_BITS.store(0.0f32.to_bits(), Ordering::SeqCst);
        FASTFORWARD_FLAG.store(true, Ordering::SeqCst);
        FASTFORWARD_NOTIFICATION.store(true, Ordering::SeqCst);
        FASTFORWARD_INHIBIT_TOGGLE.store(false, Ordering::SeqCst);

        register_environment(test_fastforward_environment);

        assert_eq!(FASTFORWARD_OVERRIDE_CALLS.load(Ordering::SeqCst), 1);
        assert_eq!(f32::from_bits(FASTFORWARD_RATIO_BITS.load(Ordering::SeqCst)), -1.0);
        assert!(!FASTFORWARD_FLAG.load(Ordering::SeqCst));
        assert!(!FASTFORWARD_NOTIFICATION.load(Ordering::SeqCst));
        assert!(FASTFORWARD_INHIBIT_TOGGLE.load(Ordering::SeqCst));
    }

    unsafe extern "C" fn test_fastforward_environment(cmd: c_uint, data: *mut c_void) -> bool {
        if cmd != RETRO_ENVIRONMENT_SET_FASTFORWARDING_OVERRIDE {
            return false;
        }

        let ff_override = unsafe { &*data.cast::<RetroFastforwardingOverride>() };
        FASTFORWARD_OVERRIDE_CALLS.fetch_add(1, Ordering::SeqCst);
        FASTFORWARD_RATIO_BITS.store(ff_override.ratio.to_bits(), Ordering::SeqCst);
        FASTFORWARD_FLAG.store(ff_override.fastforward, Ordering::SeqCst);
        FASTFORWARD_NOTIFICATION.store(ff_override.notification, Ordering::SeqCst);
        FASTFORWARD_INHIBIT_TOGGLE.store(ff_override.inhibit_toggle, Ordering::SeqCst);
        true
    }

    #[test]
    fn sound_font_options_reads_sorted_sf2_files() {
        let directory = std::env::temp_dir().join(format!("wie-libretro-sf2-scan-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(directory.join("zeta.sf2"), b"").unwrap();
        std::fs::write(directory.join("Alpha.SF2"), b"").unwrap();
        std::fs::write(directory.join("ignore.txt"), b"").unwrap();

        let options = sound_font_options(directory.clone());

        let _ = std::fs::remove_dir_all(directory);
        assert_eq!(options, vec!["Alpha.SF2", "zeta.sf2"]);
    }
}
