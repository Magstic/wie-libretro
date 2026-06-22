use std::{collections::HashMap, fs, path::Path};

use gilrs::Button;
use winit::keyboard::KeyCode as WinitKeyCode;

use wie_backend::KeyCode;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum GamepadInput {
    Button(Button),
    Axis(GamepadAxisDirection),
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum GamepadAxisDirection {
    LeftStickUp,
    LeftStickDown,
    LeftStickLeft,
    LeftStickRight,
    RightStickUp,
    RightStickDown,
    RightStickLeft,
    RightStickRight,
}

#[derive(Clone)]
pub struct Config {
    keyboard: HashMap<WinitKeyCode, KeyCode>,
    gamepad: HashMap<GamepadInput, KeyCode>,
}

enum Section {
    None,
    Keyboard,
    Gamepad,
}

const DEFAULT_KEYBOARD_MAP: &[(WinitKeyCode, KeyCode)] = &[
    (WinitKeyCode::Digit1, KeyCode::NUM1),
    (WinitKeyCode::Digit2, KeyCode::NUM2),
    (WinitKeyCode::Digit3, KeyCode::NUM3),
    (WinitKeyCode::KeyQ, KeyCode::NUM4),
    (WinitKeyCode::KeyW, KeyCode::NUM5),
    (WinitKeyCode::KeyE, KeyCode::NUM6),
    (WinitKeyCode::KeyA, KeyCode::NUM7),
    (WinitKeyCode::KeyS, KeyCode::NUM8),
    (WinitKeyCode::KeyD, KeyCode::NUM9),
    (WinitKeyCode::KeyZ, KeyCode::STAR),
    (WinitKeyCode::KeyX, KeyCode::NUM0),
    (WinitKeyCode::KeyC, KeyCode::HASH),
    (WinitKeyCode::Space, KeyCode::OK),
    (WinitKeyCode::ArrowUp, KeyCode::UP),
    (WinitKeyCode::ArrowDown, KeyCode::DOWN),
    (WinitKeyCode::ArrowLeft, KeyCode::LEFT),
    (WinitKeyCode::ArrowRight, KeyCode::RIGHT),
    (WinitKeyCode::Backspace, KeyCode::CLEAR),
    (WinitKeyCode::ShiftLeft, KeyCode::LEFT_SOFT_KEY),
    (WinitKeyCode::ShiftRight, KeyCode::RIGHT_SOFT_KEY),
    (WinitKeyCode::Backquote, KeyCode::VOLUME_UP),
    (WinitKeyCode::Tab, KeyCode::VOLUME_DOWN),
    (WinitKeyCode::F1, KeyCode::CALL),
    (WinitKeyCode::F2, KeyCode::HANGUP),
];

const DEFAULT_GAMEPAD_BUTTON_MAP: &[(Button, KeyCode)] = &[
    (Button::South, KeyCode::OK),
    (Button::East, KeyCode::CLEAR),
    (Button::West, KeyCode::NUM7),
    (Button::North, KeyCode::NUM9),
    (Button::DPadUp, KeyCode::NUM2),
    (Button::DPadDown, KeyCode::NUM8),
    (Button::DPadLeft, KeyCode::NUM4),
    (Button::DPadRight, KeyCode::NUM6),
    (Button::LeftTrigger, KeyCode::STAR),
    (Button::RightTrigger, KeyCode::HASH),
    (Button::LeftTrigger2, KeyCode::NUM1),
    (Button::RightTrigger2, KeyCode::NUM3),
    (Button::Start, KeyCode::NUM0),
];

const DEFAULT_GAMEPAD_AXIS_MAP: &[(GamepadAxisDirection, KeyCode)] = &[
    (GamepadAxisDirection::LeftStickUp, KeyCode::DOWN),
    (GamepadAxisDirection::LeftStickDown, KeyCode::UP),
    (GamepadAxisDirection::LeftStickLeft, KeyCode::LEFT),
    (GamepadAxisDirection::LeftStickRight, KeyCode::RIGHT),
];

const AVAILABLE_PHONE_KEY_NAMES: &[&str] = &[
    "UP",
    "DOWN",
    "LEFT",
    "RIGHT",
    "OK",
    "LEFT_SOFT_KEY",
    "RIGHT_SOFT_KEY",
    "CLEAR",
    "CALL",
    "HANGUP",
    "VOLUME_UP",
    "VOLUME_DOWN",
    "NUM0",
    "NUM1",
    "NUM2",
    "NUM3",
    "NUM4",
    "NUM5",
    "NUM6",
    "NUM7",
    "NUM8",
    "NUM9",
    "HASH",
    "STAR",
];

impl Config {
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        if !path.exists() {
            let config = Self::default();
            config.write_default(path)?;
            return Ok(config);
        }

        let content = fs::read_to_string(path)?;
        let mut keyboard = HashMap::new();
        let mut gamepad = HashMap::new();
        let mut section = Section::None;

        for raw_line in content.lines() {
            let line = raw_line.trim();
            if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
                continue;
            }

            match line {
                "[keyboard]" => {
                    section = Section::Keyboard;
                    continue;
                }
                "[gamepad]" => {
                    section = Section::Gamepad;
                    continue;
                }
                _ => {}
            }

            let Some((source, target)) = line.split_once('=') else {
                tracing::warn!("Ignoring malformed config line: {line}");
                continue;
            };

            let source = source.trim();
            let target = target.trim();

            match section {
                Section::Keyboard => {
                    let Some(source) = parse_winit_key_code(source) else {
                        tracing::warn!("Ignoring unknown keyboard key in config: {source}");
                        continue;
                    };
                    let Some(target) = parse_backend_key_code(target) else {
                        tracing::warn!("Ignoring unknown game key in config: {target}");
                        continue;
                    };

                    keyboard.insert(source, target);
                }
                Section::Gamepad => {
                    let Some(source) = parse_gamepad_input(source) else {
                        tracing::warn!("Ignoring unknown gamepad input in config: {source}");
                        continue;
                    };
                    let Some(target) = parse_backend_key_code(target) else {
                        tracing::warn!("Ignoring unknown game key in config: {target}");
                        continue;
                    };

                    gamepad.insert(source, target);
                }
                Section::None => {
                    tracing::warn!("Ignoring config line outside any section: {line}");
                }
            }
        }

        for (source, target) in DEFAULT_KEYBOARD_MAP {
            keyboard.entry(*source).or_insert(*target);
        }

        for (source, target) in DEFAULT_GAMEPAD_BUTTON_MAP {
            let source = GamepadInput::Button(*source);
            gamepad.entry(source).or_insert(*target);
        }

        for (source, target) in DEFAULT_GAMEPAD_AXIS_MAP {
            let source = GamepadInput::Axis(*source);
            gamepad.entry(source).or_insert(*target);
        }

        Ok(Self { keyboard, gamepad })
    }

    pub fn keyboard_map(&self) -> &HashMap<WinitKeyCode, KeyCode> {
        &self.keyboard
    }

    pub fn gamepad_map(&self) -> &HashMap<GamepadInput, KeyCode> {
        &self.gamepad
    }

    fn write_default(&self, path: &Path) -> anyhow::Result<()> {
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            fs::create_dir_all(parent)?;
        }

        fs::write(path, self.to_cfg_string())?;

        Ok(())
    }

    fn to_cfg_string(&self) -> String {
        let mut output = config_header();
        output.push_str("[keyboard]\n");
        for (source, target) in DEFAULT_KEYBOARD_MAP {
            output.push_str(&format!("{} = {}\n", format_winit_key_code(*source), format_backend_key_code(*target)));
        }
        output.push('\n');
        output.push_str("[gamepad]\n");
        for (source, target) in DEFAULT_GAMEPAD_BUTTON_MAP {
            output.push_str(&format!(
                "{} = {}\n",
                format_gamepad_input(GamepadInput::Button(*source)),
                format_backend_key_code(*target)
            ));
        }
        for (source, target) in DEFAULT_GAMEPAD_AXIS_MAP {
            output.push_str(&format!(
                "{} = {}\n",
                format_gamepad_input(GamepadInput::Axis(*source)),
                format_backend_key_code(*target)
            ));
        }
        output
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            keyboard: DEFAULT_KEYBOARD_MAP.iter().copied().collect(),
            gamepad: DEFAULT_GAMEPAD_BUTTON_MAP
                .iter()
                .map(|(source, target)| (GamepadInput::Button(*source), *target))
                .chain(
                    DEFAULT_GAMEPAD_AXIS_MAP
                        .iter()
                        .map(|(source, target)| (GamepadInput::Axis(*source), *target)),
                )
                .collect(),
        }
    }
}

fn parse_backend_key_code(value: &str) -> Option<KeyCode> {
    match value {
        "UP" => Some(KeyCode::UP),
        "DOWN" => Some(KeyCode::DOWN),
        "LEFT" => Some(KeyCode::LEFT),
        "RIGHT" => Some(KeyCode::RIGHT),
        "OK" => Some(KeyCode::OK),
        "LEFT_SOFT_KEY" => Some(KeyCode::LEFT_SOFT_KEY),
        "RIGHT_SOFT_KEY" => Some(KeyCode::RIGHT_SOFT_KEY),
        "CLEAR" | "CLR" => Some(KeyCode::CLEAR),
        "CALL" => Some(KeyCode::CALL),
        "HANGUP" => Some(KeyCode::HANGUP),
        "VOLUME_UP" => Some(KeyCode::VOLUME_UP),
        "VOLUME_DOWN" => Some(KeyCode::VOLUME_DOWN),
        "NUM0" | "0" => Some(KeyCode::NUM0),
        "NUM1" | "1" => Some(KeyCode::NUM1),
        "NUM2" | "2" => Some(KeyCode::NUM2),
        "NUM3" | "3" => Some(KeyCode::NUM3),
        "NUM4" | "4" => Some(KeyCode::NUM4),
        "NUM5" | "5" => Some(KeyCode::NUM5),
        "NUM6" | "6" => Some(KeyCode::NUM6),
        "NUM7" | "7" => Some(KeyCode::NUM7),
        "NUM8" | "8" => Some(KeyCode::NUM8),
        "NUM9" | "9" => Some(KeyCode::NUM9),
        "HASH" | "#" => Some(KeyCode::HASH),
        "STAR" | "*" => Some(KeyCode::STAR),
        _ => None,
    }
}

fn format_backend_key_code(value: KeyCode) -> &'static str {
    match value {
        KeyCode::UP => "UP",
        KeyCode::DOWN => "DOWN",
        KeyCode::LEFT => "LEFT",
        KeyCode::RIGHT => "RIGHT",
        KeyCode::OK => "OK",
        KeyCode::LEFT_SOFT_KEY => "LEFT_SOFT_KEY",
        KeyCode::RIGHT_SOFT_KEY => "RIGHT_SOFT_KEY",
        KeyCode::CLEAR => "CLEAR",
        KeyCode::CALL => "CALL",
        KeyCode::HANGUP => "HANGUP",
        KeyCode::VOLUME_UP => "VOLUME_UP",
        KeyCode::VOLUME_DOWN => "VOLUME_DOWN",
        KeyCode::NUM0 => "NUM0",
        KeyCode::NUM1 => "NUM1",
        KeyCode::NUM2 => "NUM2",
        KeyCode::NUM3 => "NUM3",
        KeyCode::NUM4 => "NUM4",
        KeyCode::NUM5 => "NUM5",
        KeyCode::NUM6 => "NUM6",
        KeyCode::NUM7 => "NUM7",
        KeyCode::NUM8 => "NUM8",
        KeyCode::NUM9 => "NUM9",
        KeyCode::HASH => "HASH",
        KeyCode::STAR => "STAR",
    }
}

fn parse_gamepad_input(value: &str) -> Option<GamepadInput> {
    parse_gamepad_button(value)
        .map(GamepadInput::Button)
        .or_else(|| parse_gamepad_axis_direction(value).map(GamepadInput::Axis))
}

fn parse_gamepad_button(value: &str) -> Option<Button> {
    match value {
        "South" => Some(Button::South),
        "East" => Some(Button::East),
        "North" => Some(Button::North),
        "West" => Some(Button::West),
        "C" => Some(Button::C),
        "Z" => Some(Button::Z),
        "LeftTrigger" => Some(Button::LeftTrigger),
        "LeftTrigger2" => Some(Button::LeftTrigger2),
        "RightTrigger" => Some(Button::RightTrigger),
        "RightTrigger2" => Some(Button::RightTrigger2),
        "Select" => Some(Button::Select),
        "Start" => Some(Button::Start),
        "Mode" => Some(Button::Mode),
        "LeftThumb" => Some(Button::LeftThumb),
        "RightThumb" => Some(Button::RightThumb),
        "DPadUp" => Some(Button::DPadUp),
        "DPadDown" => Some(Button::DPadDown),
        "DPadLeft" => Some(Button::DPadLeft),
        "DPadRight" => Some(Button::DPadRight),
        _ => None,
    }
}

fn parse_gamepad_axis_direction(value: &str) -> Option<GamepadAxisDirection> {
    match value {
        "LeftStickUp" => Some(GamepadAxisDirection::LeftStickUp),
        "LeftStickDown" => Some(GamepadAxisDirection::LeftStickDown),
        "LeftStickLeft" => Some(GamepadAxisDirection::LeftStickLeft),
        "LeftStickRight" => Some(GamepadAxisDirection::LeftStickRight),
        "RightStickUp" => Some(GamepadAxisDirection::RightStickUp),
        "RightStickDown" => Some(GamepadAxisDirection::RightStickDown),
        "RightStickLeft" => Some(GamepadAxisDirection::RightStickLeft),
        "RightStickRight" => Some(GamepadAxisDirection::RightStickRight),
        _ => None,
    }
}

fn format_gamepad_input(value: GamepadInput) -> &'static str {
    match value {
        GamepadInput::Button(button) => format_gamepad_button(button),
        GamepadInput::Axis(axis) => format_gamepad_axis_direction(axis),
    }
}

fn format_gamepad_button(value: Button) -> &'static str {
    match value {
        Button::South => "South",
        Button::East => "East",
        Button::North => "North",
        Button::West => "West",
        Button::C => "C",
        Button::Z => "Z",
        Button::LeftTrigger => "LeftTrigger",
        Button::LeftTrigger2 => "LeftTrigger2",
        Button::RightTrigger => "RightTrigger",
        Button::RightTrigger2 => "RightTrigger2",
        Button::Select => "Select",
        Button::Start => "Start",
        Button::Mode => "Mode",
        Button::LeftThumb => "LeftThumb",
        Button::RightThumb => "RightThumb",
        Button::DPadUp => "DPadUp",
        Button::DPadDown => "DPadDown",
        Button::DPadLeft => "DPadLeft",
        Button::DPadRight => "DPadRight",
        Button::Unknown => "Unknown",
    }
}

fn format_gamepad_axis_direction(value: GamepadAxisDirection) -> &'static str {
    match value {
        GamepadAxisDirection::LeftStickUp => "LeftStickUp",
        GamepadAxisDirection::LeftStickDown => "LeftStickDown",
        GamepadAxisDirection::LeftStickLeft => "LeftStickLeft",
        GamepadAxisDirection::LeftStickRight => "LeftStickRight",
        GamepadAxisDirection::RightStickUp => "RightStickUp",
        GamepadAxisDirection::RightStickDown => "RightStickDown",
        GamepadAxisDirection::RightStickLeft => "RightStickLeft",
        GamepadAxisDirection::RightStickRight => "RightStickRight",
    }
}

fn config_header() -> String {
    let mut output = String::from("# Available mobile key names:\n");
    output.push_str("# ");
    output.push_str(&AVAILABLE_PHONE_KEY_NAMES.join(" "));
    output.push('\n');
    output.push_str("# Keyboard values use Winit physical key names.\n");
    output.push_str("# Gamepad values use GilRs button names plus axis directions.\n");
    output.push_str(
        "# Axis names: LeftStickUp LeftStickDown LeftStickLeft LeftStickRight RightStickUp RightStickDown RightStickLeft RightStickRight\n\n",
    );
    output
}

fn parse_winit_key_code(value: &str) -> Option<WinitKeyCode> {
    match value {
        "KeyA" => Some(WinitKeyCode::KeyA),
        "KeyB" => Some(WinitKeyCode::KeyB),
        "KeyC" => Some(WinitKeyCode::KeyC),
        "KeyD" => Some(WinitKeyCode::KeyD),
        "KeyE" => Some(WinitKeyCode::KeyE),
        "KeyF" => Some(WinitKeyCode::KeyF),
        "KeyG" => Some(WinitKeyCode::KeyG),
        "KeyH" => Some(WinitKeyCode::KeyH),
        "KeyI" => Some(WinitKeyCode::KeyI),
        "KeyJ" => Some(WinitKeyCode::KeyJ),
        "KeyK" => Some(WinitKeyCode::KeyK),
        "KeyL" => Some(WinitKeyCode::KeyL),
        "KeyM" => Some(WinitKeyCode::KeyM),
        "KeyN" => Some(WinitKeyCode::KeyN),
        "KeyO" => Some(WinitKeyCode::KeyO),
        "KeyP" => Some(WinitKeyCode::KeyP),
        "KeyQ" => Some(WinitKeyCode::KeyQ),
        "KeyR" => Some(WinitKeyCode::KeyR),
        "KeyS" => Some(WinitKeyCode::KeyS),
        "KeyT" => Some(WinitKeyCode::KeyT),
        "KeyU" => Some(WinitKeyCode::KeyU),
        "KeyV" => Some(WinitKeyCode::KeyV),
        "KeyW" => Some(WinitKeyCode::KeyW),
        "KeyX" => Some(WinitKeyCode::KeyX),
        "KeyY" => Some(WinitKeyCode::KeyY),
        "KeyZ" => Some(WinitKeyCode::KeyZ),
        "Digit0" => Some(WinitKeyCode::Digit0),
        "Digit1" => Some(WinitKeyCode::Digit1),
        "Digit2" => Some(WinitKeyCode::Digit2),
        "Digit3" => Some(WinitKeyCode::Digit3),
        "Digit4" => Some(WinitKeyCode::Digit4),
        "Digit5" => Some(WinitKeyCode::Digit5),
        "Digit6" => Some(WinitKeyCode::Digit6),
        "Digit7" => Some(WinitKeyCode::Digit7),
        "Digit8" => Some(WinitKeyCode::Digit8),
        "Digit9" => Some(WinitKeyCode::Digit9),
        "Space" => Some(WinitKeyCode::Space),
        "Tab" => Some(WinitKeyCode::Tab),
        "Enter" => Some(WinitKeyCode::Enter),
        "Escape" => Some(WinitKeyCode::Escape),
        "Backspace" => Some(WinitKeyCode::Backspace),
        "Backquote" => Some(WinitKeyCode::Backquote),
        "Minus" => Some(WinitKeyCode::Minus),
        "Equal" => Some(WinitKeyCode::Equal),
        "BracketLeft" => Some(WinitKeyCode::BracketLeft),
        "BracketRight" => Some(WinitKeyCode::BracketRight),
        "Backslash" => Some(WinitKeyCode::Backslash),
        "Semicolon" => Some(WinitKeyCode::Semicolon),
        "Quote" => Some(WinitKeyCode::Quote),
        "Comma" => Some(WinitKeyCode::Comma),
        "Period" => Some(WinitKeyCode::Period),
        "Slash" => Some(WinitKeyCode::Slash),
        "ShiftLeft" => Some(WinitKeyCode::ShiftLeft),
        "ShiftRight" => Some(WinitKeyCode::ShiftRight),
        "ControlLeft" => Some(WinitKeyCode::ControlLeft),
        "ControlRight" => Some(WinitKeyCode::ControlRight),
        "AltLeft" => Some(WinitKeyCode::AltLeft),
        "AltRight" => Some(WinitKeyCode::AltRight),
        "SuperLeft" => Some(WinitKeyCode::SuperLeft),
        "SuperRight" => Some(WinitKeyCode::SuperRight),
        "CapsLock" => Some(WinitKeyCode::CapsLock),
        "NumLock" => Some(WinitKeyCode::NumLock),
        "ScrollLock" => Some(WinitKeyCode::ScrollLock),
        "PrintScreen" => Some(WinitKeyCode::PrintScreen),
        "Pause" => Some(WinitKeyCode::Pause),
        "Insert" => Some(WinitKeyCode::Insert),
        "Delete" => Some(WinitKeyCode::Delete),
        "Home" => Some(WinitKeyCode::Home),
        "End" => Some(WinitKeyCode::End),
        "PageUp" => Some(WinitKeyCode::PageUp),
        "PageDown" => Some(WinitKeyCode::PageDown),
        "ArrowUp" => Some(WinitKeyCode::ArrowUp),
        "ArrowDown" => Some(WinitKeyCode::ArrowDown),
        "ArrowLeft" => Some(WinitKeyCode::ArrowLeft),
        "ArrowRight" => Some(WinitKeyCode::ArrowRight),
        "Numpad0" => Some(WinitKeyCode::Numpad0),
        "Numpad1" => Some(WinitKeyCode::Numpad1),
        "Numpad2" => Some(WinitKeyCode::Numpad2),
        "Numpad3" => Some(WinitKeyCode::Numpad3),
        "Numpad4" => Some(WinitKeyCode::Numpad4),
        "Numpad5" => Some(WinitKeyCode::Numpad5),
        "Numpad6" => Some(WinitKeyCode::Numpad6),
        "Numpad7" => Some(WinitKeyCode::Numpad7),
        "Numpad8" => Some(WinitKeyCode::Numpad8),
        "Numpad9" => Some(WinitKeyCode::Numpad9),
        "NumpadAdd" => Some(WinitKeyCode::NumpadAdd),
        "NumpadSubtract" => Some(WinitKeyCode::NumpadSubtract),
        "NumpadMultiply" => Some(WinitKeyCode::NumpadMultiply),
        "NumpadDivide" => Some(WinitKeyCode::NumpadDivide),
        "NumpadDecimal" => Some(WinitKeyCode::NumpadDecimal),
        "NumpadEnter" => Some(WinitKeyCode::NumpadEnter),
        "F1" => Some(WinitKeyCode::F1),
        "F2" => Some(WinitKeyCode::F2),
        "F3" => Some(WinitKeyCode::F3),
        "F4" => Some(WinitKeyCode::F4),
        "F5" => Some(WinitKeyCode::F5),
        "F6" => Some(WinitKeyCode::F6),
        "F7" => Some(WinitKeyCode::F7),
        "F8" => Some(WinitKeyCode::F8),
        "F9" => Some(WinitKeyCode::F9),
        "F10" => Some(WinitKeyCode::F10),
        "F11" => Some(WinitKeyCode::F11),
        "F12" => Some(WinitKeyCode::F12),
        "F13" => Some(WinitKeyCode::F13),
        "F14" => Some(WinitKeyCode::F14),
        "F15" => Some(WinitKeyCode::F15),
        "F16" => Some(WinitKeyCode::F16),
        "F17" => Some(WinitKeyCode::F17),
        "F18" => Some(WinitKeyCode::F18),
        "F19" => Some(WinitKeyCode::F19),
        "F20" => Some(WinitKeyCode::F20),
        "F21" => Some(WinitKeyCode::F21),
        "F22" => Some(WinitKeyCode::F22),
        "F23" => Some(WinitKeyCode::F23),
        "F24" => Some(WinitKeyCode::F24),
        _ => None,
    }
}

fn format_winit_key_code(value: WinitKeyCode) -> &'static str {
    match value {
        WinitKeyCode::KeyA => "KeyA",
        WinitKeyCode::KeyB => "KeyB",
        WinitKeyCode::KeyC => "KeyC",
        WinitKeyCode::KeyD => "KeyD",
        WinitKeyCode::KeyE => "KeyE",
        WinitKeyCode::KeyF => "KeyF",
        WinitKeyCode::KeyG => "KeyG",
        WinitKeyCode::KeyH => "KeyH",
        WinitKeyCode::KeyI => "KeyI",
        WinitKeyCode::KeyJ => "KeyJ",
        WinitKeyCode::KeyK => "KeyK",
        WinitKeyCode::KeyL => "KeyL",
        WinitKeyCode::KeyM => "KeyM",
        WinitKeyCode::KeyN => "KeyN",
        WinitKeyCode::KeyO => "KeyO",
        WinitKeyCode::KeyP => "KeyP",
        WinitKeyCode::KeyQ => "KeyQ",
        WinitKeyCode::KeyR => "KeyR",
        WinitKeyCode::KeyS => "KeyS",
        WinitKeyCode::KeyT => "KeyT",
        WinitKeyCode::KeyU => "KeyU",
        WinitKeyCode::KeyV => "KeyV",
        WinitKeyCode::KeyW => "KeyW",
        WinitKeyCode::KeyX => "KeyX",
        WinitKeyCode::KeyY => "KeyY",
        WinitKeyCode::KeyZ => "KeyZ",
        WinitKeyCode::Digit0 => "Digit0",
        WinitKeyCode::Digit1 => "Digit1",
        WinitKeyCode::Digit2 => "Digit2",
        WinitKeyCode::Digit3 => "Digit3",
        WinitKeyCode::Digit4 => "Digit4",
        WinitKeyCode::Digit5 => "Digit5",
        WinitKeyCode::Digit6 => "Digit6",
        WinitKeyCode::Digit7 => "Digit7",
        WinitKeyCode::Digit8 => "Digit8",
        WinitKeyCode::Digit9 => "Digit9",
        WinitKeyCode::Space => "Space",
        WinitKeyCode::Tab => "Tab",
        WinitKeyCode::Enter => "Enter",
        WinitKeyCode::Escape => "Escape",
        WinitKeyCode::Backspace => "Backspace",
        WinitKeyCode::Backquote => "Backquote",
        WinitKeyCode::Minus => "Minus",
        WinitKeyCode::Equal => "Equal",
        WinitKeyCode::BracketLeft => "BracketLeft",
        WinitKeyCode::BracketRight => "BracketRight",
        WinitKeyCode::Backslash => "Backslash",
        WinitKeyCode::Semicolon => "Semicolon",
        WinitKeyCode::Quote => "Quote",
        WinitKeyCode::Comma => "Comma",
        WinitKeyCode::Period => "Period",
        WinitKeyCode::Slash => "Slash",
        WinitKeyCode::ShiftLeft => "ShiftLeft",
        WinitKeyCode::ShiftRight => "ShiftRight",
        WinitKeyCode::ControlLeft => "ControlLeft",
        WinitKeyCode::ControlRight => "ControlRight",
        WinitKeyCode::AltLeft => "AltLeft",
        WinitKeyCode::AltRight => "AltRight",
        WinitKeyCode::SuperLeft => "SuperLeft",
        WinitKeyCode::SuperRight => "SuperRight",
        WinitKeyCode::CapsLock => "CapsLock",
        WinitKeyCode::NumLock => "NumLock",
        WinitKeyCode::ScrollLock => "ScrollLock",
        WinitKeyCode::PrintScreen => "PrintScreen",
        WinitKeyCode::Pause => "Pause",
        WinitKeyCode::Insert => "Insert",
        WinitKeyCode::Delete => "Delete",
        WinitKeyCode::Home => "Home",
        WinitKeyCode::End => "End",
        WinitKeyCode::PageUp => "PageUp",
        WinitKeyCode::PageDown => "PageDown",
        WinitKeyCode::ArrowUp => "ArrowUp",
        WinitKeyCode::ArrowDown => "ArrowDown",
        WinitKeyCode::ArrowLeft => "ArrowLeft",
        WinitKeyCode::ArrowRight => "ArrowRight",
        WinitKeyCode::Numpad0 => "Numpad0",
        WinitKeyCode::Numpad1 => "Numpad1",
        WinitKeyCode::Numpad2 => "Numpad2",
        WinitKeyCode::Numpad3 => "Numpad3",
        WinitKeyCode::Numpad4 => "Numpad4",
        WinitKeyCode::Numpad5 => "Numpad5",
        WinitKeyCode::Numpad6 => "Numpad6",
        WinitKeyCode::Numpad7 => "Numpad7",
        WinitKeyCode::Numpad8 => "Numpad8",
        WinitKeyCode::Numpad9 => "Numpad9",
        WinitKeyCode::NumpadAdd => "NumpadAdd",
        WinitKeyCode::NumpadSubtract => "NumpadSubtract",
        WinitKeyCode::NumpadMultiply => "NumpadMultiply",
        WinitKeyCode::NumpadDivide => "NumpadDivide",
        WinitKeyCode::NumpadDecimal => "NumpadDecimal",
        WinitKeyCode::NumpadEnter => "NumpadEnter",
        WinitKeyCode::F1 => "F1",
        WinitKeyCode::F2 => "F2",
        WinitKeyCode::F3 => "F3",
        WinitKeyCode::F4 => "F4",
        WinitKeyCode::F5 => "F5",
        WinitKeyCode::F6 => "F6",
        WinitKeyCode::F7 => "F7",
        WinitKeyCode::F8 => "F8",
        WinitKeyCode::F9 => "F9",
        WinitKeyCode::F10 => "F10",
        WinitKeyCode::F11 => "F11",
        WinitKeyCode::F12 => "F12",
        WinitKeyCode::F13 => "F13",
        WinitKeyCode::F14 => "F14",
        WinitKeyCode::F15 => "F15",
        WinitKeyCode::F16 => "F16",
        WinitKeyCode::F17 => "F17",
        WinitKeyCode::F18 => "F18",
        WinitKeyCode::F19 => "F19",
        WinitKeyCode::F20 => "F20",
        WinitKeyCode::F21 => "F21",
        WinitKeyCode::F22 => "F22",
        WinitKeyCode::F23 => "F23",
        WinitKeyCode::F24 => "F24",
        _ => "Unsupported",
    }
}
