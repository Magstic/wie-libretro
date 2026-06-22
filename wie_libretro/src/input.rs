use std::{collections::HashMap, ffi::c_char};

use wie_backend::{Event, KeyCode};

use crate::ffi::{
    RETRO_DEVICE_ANALOG, RETRO_DEVICE_ID_ANALOG_X, RETRO_DEVICE_ID_ANALOG_Y, RETRO_DEVICE_ID_JOYPAD_A, RETRO_DEVICE_ID_JOYPAD_B,
    RETRO_DEVICE_ID_JOYPAD_DOWN, RETRO_DEVICE_ID_JOYPAD_L, RETRO_DEVICE_ID_JOYPAD_L2, RETRO_DEVICE_ID_JOYPAD_LEFT, RETRO_DEVICE_ID_JOYPAD_R,
    RETRO_DEVICE_ID_JOYPAD_R2, RETRO_DEVICE_ID_JOYPAD_RIGHT, RETRO_DEVICE_ID_JOYPAD_START, RETRO_DEVICE_ID_JOYPAD_UP, RETRO_DEVICE_ID_JOYPAD_X,
    RETRO_DEVICE_ID_JOYPAD_Y, RETRO_DEVICE_INDEX_ANALOG_LEFT, RETRO_DEVICE_JOYPAD, RETRO_DEVICE_KEYBOARD, RETROK_0, RETROK_1, RETROK_2, RETROK_3,
    RETROK_4, RETROK_5, RETROK_6, RETROK_7, RETROK_8, RETROK_9, RETROK_ASTERISK, RETROK_BACKSPACE, RETROK_DOWN, RETROK_ESCAPE, RETROK_F1, RETROK_F2,
    RETROK_HASH, RETROK_LEFT, RETROK_PAGEDOWN, RETROK_PAGEUP, RETROK_RETURN, RETROK_RIGHT, RETROK_SPACE, RETROK_UP, RetroInputDescriptor,
    RetroInputPollT, RetroInputStateT,
};

const ANALOG_DEADZONE: i16 = 16_384;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct InputSource {
    device: u32,
    index: u32,
    id: u32,
    direction: i8,
}

#[derive(Clone, Copy)]
struct InputBinding {
    device: u32,
    index: u32,
    id: u32,
    direction: i8,
    key: KeyCode,
    description: &'static [u8],
}

#[derive(Default)]
pub struct InputManager {
    sources: HashMap<InputSource, KeyCode>,
    pressed_counts: HashMap<KeyCode, usize>,
}

impl InputManager {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn poll(&mut self, input_poll: Option<RetroInputPollT>, input_state: Option<RetroInputStateT>) -> Vec<Event> {
        let Some(input_state) = input_state else {
            return Vec::new();
        };

        if let Some(input_poll) = input_poll {
            unsafe { input_poll() };
        }

        let mut events = Vec::new();
        for binding in bindings() {
            let value = unsafe { input_state(0, binding.device, binding.index, binding.id) };
            let down = binding.is_down(value);
            let source = InputSource {
                device: binding.device,
                index: binding.index,
                id: binding.id,
                direction: binding.direction,
            };
            match (self.sources.contains_key(&source), down) {
                (false, true) if self.press(source, binding.key) => {
                    events.push(Event::Keydown(binding.key));
                }
                (true, false) => {
                    if let Some(key) = self.release(source) {
                        events.push(Event::Keyup(key));
                    }
                }
                _ => {}
            }
        }

        events
    }

    fn press(&mut self, source: InputSource, key: KeyCode) -> bool {
        if self.sources.insert(source, key).is_some() {
            return false;
        }

        let count = self.pressed_counts.entry(key).or_default();
        let send_keydown = *count == 0;
        *count += 1;

        send_keydown
    }

    fn release(&mut self, source: InputSource) -> Option<KeyCode> {
        let key = self.sources.remove(&source)?;
        let count = self.pressed_counts.get_mut(&key)?;
        if *count > 1 {
            *count -= 1;
            return None;
        }

        self.pressed_counts.remove(&key);
        Some(key)
    }
}

pub fn input_descriptors() -> Vec<RetroInputDescriptor> {
    let mut descriptors = DEFAULT_JOYPAD_BINDINGS
        .iter()
        .map(|binding| RetroInputDescriptor {
            port: 0,
            device: binding.device,
            index: binding.index,
            id: binding.id,
            description: c_ptr(binding.description),
        })
        .collect::<Vec<_>>();

    descriptors.push(RetroInputDescriptor {
        port: 0,
        device: 0,
        index: 0,
        id: 0,
        description: std::ptr::null(),
    });
    descriptors
}

fn bindings() -> Vec<InputBinding> {
    let mut bindings = DEFAULT_JOYPAD_BINDINGS.to_vec();
    bindings.extend_from_slice(keyboard_bindings());
    bindings
}

fn keyboard_bindings() -> &'static [InputBinding] {
    KEYBOARD_BINDINGS
}

fn c_ptr(bytes: &'static [u8]) -> *const c_char {
    bytes.as_ptr().cast::<c_char>()
}

const DEFAULT_JOYPAD_BINDINGS: &[InputBinding] = &[
    analog(RETRO_DEVICE_ID_ANALOG_Y, -1, KeyCode::UP, b"Navigation Up\0"),
    analog(RETRO_DEVICE_ID_ANALOG_Y, 1, KeyCode::DOWN, b"Navigation Down\0"),
    analog(RETRO_DEVICE_ID_ANALOG_X, -1, KeyCode::LEFT, b"Navigation Left\0"),
    analog(RETRO_DEVICE_ID_ANALOG_X, 1, KeyCode::RIGHT, b"Navigation Right\0"),
    joy(RETRO_DEVICE_ID_JOYPAD_UP, KeyCode::NUM2, b"2\0"),
    joy(RETRO_DEVICE_ID_JOYPAD_DOWN, KeyCode::NUM8, b"8\0"),
    joy(RETRO_DEVICE_ID_JOYPAD_LEFT, KeyCode::NUM4, b"4\0"),
    joy(RETRO_DEVICE_ID_JOYPAD_RIGHT, KeyCode::NUM6, b"6\0"),
    joy(RETRO_DEVICE_ID_JOYPAD_B, KeyCode::OK, b"OK\0"),
    joy(RETRO_DEVICE_ID_JOYPAD_A, KeyCode::CLEAR, b"Clear\0"),
    joy(RETRO_DEVICE_ID_JOYPAD_Y, KeyCode::NUM7, b"7\0"),
    joy(RETRO_DEVICE_ID_JOYPAD_X, KeyCode::NUM9, b"9\0"),
    joy(RETRO_DEVICE_ID_JOYPAD_L, KeyCode::STAR, b"Star\0"),
    joy(RETRO_DEVICE_ID_JOYPAD_R, KeyCode::HASH, b"Hash\0"),
    joy(RETRO_DEVICE_ID_JOYPAD_L2, KeyCode::NUM1, b"1\0"),
    joy(RETRO_DEVICE_ID_JOYPAD_R2, KeyCode::NUM3, b"3\0"),
    joy(RETRO_DEVICE_ID_JOYPAD_START, KeyCode::NUM0, b"0\0"),
];

const KEYBOARD_BINDINGS: &[InputBinding] = &[
    key(RETROK_UP, KeyCode::UP),
    key(RETROK_DOWN, KeyCode::DOWN),
    key(RETROK_LEFT, KeyCode::LEFT),
    key(RETROK_RIGHT, KeyCode::RIGHT),
    key(RETROK_RETURN, KeyCode::OK),
    key(RETROK_BACKSPACE, KeyCode::CLEAR),
    key(RETROK_F1, KeyCode::LEFT_SOFT_KEY),
    key(RETROK_F2, KeyCode::RIGHT_SOFT_KEY),
    key(RETROK_SPACE, KeyCode::CALL),
    key(RETROK_ESCAPE, KeyCode::HANGUP),
    key(RETROK_PAGEUP, KeyCode::VOLUME_UP),
    key(RETROK_PAGEDOWN, KeyCode::VOLUME_DOWN),
    key(RETROK_0, KeyCode::NUM0),
    key(RETROK_1, KeyCode::NUM1),
    key(RETROK_2, KeyCode::NUM2),
    key(RETROK_3, KeyCode::NUM3),
    key(RETROK_4, KeyCode::NUM4),
    key(RETROK_5, KeyCode::NUM5),
    key(RETROK_6, KeyCode::NUM6),
    key(RETROK_7, KeyCode::NUM7),
    key(RETROK_8, KeyCode::NUM8),
    key(RETROK_9, KeyCode::NUM9),
    key(RETROK_ASTERISK, KeyCode::STAR),
    key(RETROK_HASH, KeyCode::HASH),
];

const fn joy(id: u32, key: KeyCode, description: &'static [u8]) -> InputBinding {
    InputBinding {
        device: RETRO_DEVICE_JOYPAD,
        index: 0,
        id,
        direction: 0,
        key,
        description,
    }
}

const fn analog(id: u32, direction: i8, key: KeyCode, description: &'static [u8]) -> InputBinding {
    InputBinding {
        device: RETRO_DEVICE_ANALOG,
        index: RETRO_DEVICE_INDEX_ANALOG_LEFT,
        id,
        direction,
        key,
        description,
    }
}

const fn key(id: u32, key: KeyCode) -> InputBinding {
    InputBinding {
        device: RETRO_DEVICE_KEYBOARD,
        index: 0,
        id,
        direction: 0,
        key,
        description: b"\0",
    }
}

impl InputBinding {
    fn is_down(&self, value: i16) -> bool {
        match self.direction {
            -1 => value <= -ANALOG_DEADZONE,
            1 => value >= ANALOG_DEADZONE,
            _ => value != 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use wie_backend::KeyCode;

    use crate::ffi::{
        RETRO_DEVICE_ID_ANALOG_X, RETRO_DEVICE_ID_ANALOG_Y, RETRO_DEVICE_ID_JOYPAD_A, RETRO_DEVICE_ID_JOYPAD_B, RETRO_DEVICE_ID_JOYPAD_DOWN,
        RETRO_DEVICE_ID_JOYPAD_L, RETRO_DEVICE_ID_JOYPAD_L2, RETRO_DEVICE_ID_JOYPAD_LEFT, RETRO_DEVICE_ID_JOYPAD_R, RETRO_DEVICE_ID_JOYPAD_R2,
        RETRO_DEVICE_ID_JOYPAD_RIGHT, RETRO_DEVICE_ID_JOYPAD_SELECT, RETRO_DEVICE_ID_JOYPAD_START, RETRO_DEVICE_ID_JOYPAD_UP,
        RETRO_DEVICE_ID_JOYPAD_X, RETRO_DEVICE_ID_JOYPAD_Y,
    };

    use super::{DEFAULT_JOYPAD_BINDINGS, KEYBOARD_BINDINGS};

    #[test]
    fn keyboard_completes_phone_keypad() {
        assert!(KEYBOARD_BINDINGS.len() >= 24);
    }

    #[test]
    fn joypad_descriptors_are_terminated_elsewhere() {
        assert_eq!(DEFAULT_JOYPAD_BINDINGS.len(), 17);
    }

    #[test]
    fn default_joypad_layout_matches_lgt_phone_keys() {
        assert_joy(RETRO_DEVICE_ID_JOYPAD_UP, KeyCode::NUM2);
        assert_joy(RETRO_DEVICE_ID_JOYPAD_DOWN, KeyCode::NUM8);
        assert_joy(RETRO_DEVICE_ID_JOYPAD_LEFT, KeyCode::NUM4);
        assert_joy(RETRO_DEVICE_ID_JOYPAD_RIGHT, KeyCode::NUM6);
        assert_joy(RETRO_DEVICE_ID_JOYPAD_B, KeyCode::OK);
        assert_joy(RETRO_DEVICE_ID_JOYPAD_A, KeyCode::CLEAR);
        assert_joy(RETRO_DEVICE_ID_JOYPAD_Y, KeyCode::NUM7);
        assert_joy(RETRO_DEVICE_ID_JOYPAD_X, KeyCode::NUM9);
        assert_joy(RETRO_DEVICE_ID_JOYPAD_L, KeyCode::STAR);
        assert_joy(RETRO_DEVICE_ID_JOYPAD_R, KeyCode::HASH);
        assert_joy(RETRO_DEVICE_ID_JOYPAD_L2, KeyCode::NUM1);
        assert_joy(RETRO_DEVICE_ID_JOYPAD_R2, KeyCode::NUM3);
        assert_joy(RETRO_DEVICE_ID_JOYPAD_START, KeyCode::NUM0);
        assert!(DEFAULT_JOYPAD_BINDINGS.iter().all(|binding| binding.id != RETRO_DEVICE_ID_JOYPAD_SELECT));
        assert_analog(RETRO_DEVICE_ID_ANALOG_Y, -1, KeyCode::UP);
        assert_analog(RETRO_DEVICE_ID_ANALOG_Y, 1, KeyCode::DOWN);
        assert_analog(RETRO_DEVICE_ID_ANALOG_X, -1, KeyCode::LEFT);
        assert_analog(RETRO_DEVICE_ID_ANALOG_X, 1, KeyCode::RIGHT);
    }

    fn assert_joy(id: u32, key: KeyCode) {
        assert!(
            DEFAULT_JOYPAD_BINDINGS
                .iter()
                .any(|binding| binding.device == crate::ffi::RETRO_DEVICE_JOYPAD && binding.id == id && binding.key == key)
        );
    }

    fn assert_analog(id: u32, direction: i8, key: KeyCode) {
        assert!(
            DEFAULT_JOYPAD_BINDINGS
                .iter()
                .any(|binding| binding.device == crate::ffi::RETRO_DEVICE_ANALOG
                    && binding.id == id
                    && binding.direction == direction
                    && binding.key == key)
        );
    }
}
