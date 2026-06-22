use std::{
    collections::{HashMap, HashSet},
    sync::mpsc::{Receiver, TryRecvError},
    time::{Duration, Instant},
};

use gilrs::{
    Axis, EventType, GamepadId, Gilrs,
    ff::{BaseEffect, BaseEffectType, Effect, EffectBuilder, Replay, Ticks},
};

use crate::config::{GamepadAxisDirection, GamepadInput};

const AXIS_PRESS_THRESHOLD: f32 = 0.5;
const AXIS_RELEASE_THRESHOLD: f32 = 0.3;

pub enum GamepadCallbackEvent {
    Keydown { id: GamepadId, input: GamepadInput },
    Keyup { id: GamepadId, input: GamepadInput },
}

pub struct GamepadState {
    gilrs: Gilrs,
    vibrate_rx: Receiver<(u64, u8)>,
    pressed_inputs: HashSet<(GamepadId, GamepadInput)>,
    stick_states: HashMap<(GamepadId, StickKind), StickState>,
    active_effects: Vec<(Instant, Effect)>,
}

#[derive(Clone, Copy, Debug, Default)]
struct StickState {
    x: f32,
    y: f32,
    active_direction: Option<GamepadAxisDirection>,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum StickKind {
    Left,
    Right,
}

#[derive(Clone, Copy, Debug)]
enum StickAxisComponent {
    X,
    Y,
}

impl GamepadState {
    pub fn new(vibrate_rx: Receiver<(u64, u8)>) -> anyhow::Result<Self> {
        let gilrs = Gilrs::new().map_err(|err| anyhow::anyhow!(err.to_string()))?;

        Ok(Self {
            gilrs,
            vibrate_rx,
            pressed_inputs: HashSet::new(),
            stick_states: HashMap::new(),
            active_effects: Vec::new(),
        })
    }

    pub fn poll(&mut self) -> Vec<GamepadCallbackEvent> {
        self.cleanup_effects();
        self.consume_vibration_requests();

        let mut events = Vec::new();

        while let Some(event) = self.gilrs.next_event() {
            match event.event {
                EventType::ButtonPressed(button, _) => {
                    let input = GamepadInput::Button(button);
                    self.pressed_inputs.insert((event.id, input));
                    events.push(GamepadCallbackEvent::Keydown { id: event.id, input });
                }
                EventType::ButtonReleased(button, _) => {
                    let input = GamepadInput::Button(button);
                    self.pressed_inputs.remove(&(event.id, input));
                    events.push(GamepadCallbackEvent::Keyup { id: event.id, input });
                }
                EventType::AxisChanged(axis, value, _) => {
                    events.extend(self.handle_axis_change(event.id, axis, value));
                }
                EventType::Disconnected => {
                    self.release_stick_states(event.id);
                    let released = self.release_gamepad_inputs(event.id);
                    events.extend(released);
                }
                _ => {}
            }
        }

        events
    }

    fn release_gamepad_inputs(&mut self, id: GamepadId) -> Vec<GamepadCallbackEvent> {
        let mut events = Vec::new();
        self.pressed_inputs.retain(|(gamepad_id, input)| {
            if *gamepad_id == id {
                events.push(GamepadCallbackEvent::Keyup {
                    id: *gamepad_id,
                    input: *input,
                });
                false
            } else {
                true
            }
        });
        events
    }

    fn handle_axis_change(&mut self, id: GamepadId, axis: Axis, value: f32) -> Vec<GamepadCallbackEvent> {
        let Some((stick, component)) = stick_axis(axis) else {
            return Vec::new();
        };

        let (previous_direction, next_direction) = {
            let state = self.stick_states.entry((id, stick)).or_default();
            match component {
                StickAxisComponent::X => state.x = value,
                StickAxisComponent::Y => state.y = value,
            }

            let previous_direction = state.active_direction;
            let next_direction = resolve_stick_direction(stick, state.x, state.y, previous_direction);
            state.active_direction = next_direction;
            (previous_direction, next_direction)
        };

        self.transition_stick_direction(id, previous_direction, next_direction)
    }

    fn transition_stick_direction(
        &mut self,
        id: GamepadId,
        previous_direction: Option<GamepadAxisDirection>,
        next_direction: Option<GamepadAxisDirection>,
    ) -> Vec<GamepadCallbackEvent> {
        if previous_direction == next_direction {
            return Vec::new();
        }

        let mut events = Vec::new();

        if let Some(direction) = previous_direction {
            let input = GamepadInput::Axis(direction);
            self.pressed_inputs.remove(&(id, input));
            events.push(GamepadCallbackEvent::Keyup { id, input });
        }

        if let Some(direction) = next_direction {
            let input = GamepadInput::Axis(direction);
            self.pressed_inputs.insert((id, input));
            events.push(GamepadCallbackEvent::Keydown { id, input });
        }

        events
    }

    fn release_stick_states(&mut self, id: GamepadId) {
        self.stick_states.retain(|(gamepad_id, _), _| *gamepad_id != id);
    }

    fn cleanup_effects(&mut self) {
        let now = Instant::now();
        self.active_effects.retain(|(expires_at, _)| *expires_at > now);
    }

    fn consume_vibration_requests(&mut self) {
        loop {
            match self.vibrate_rx.try_recv() {
                Ok((duration_ms, intensity)) => self.play_vibration(duration_ms, intensity),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => break,
            }
        }
    }

    fn play_vibration(&mut self, duration_ms: u64, intensity: u8) {
        let supported = self
            .gilrs
            .gamepads()
            .filter_map(|(id, gamepad)| if gamepad.is_ff_supported() { Some(id) } else { None })
            .collect::<Vec<_>>();

        if supported.is_empty() {
            return;
        }

        let magnitude = ((u16::MAX as u32) * intensity as u32 / 100) as u16;
        if magnitude == 0 {
            return;
        }

        let duration_ms = duration_ms.clamp(1, u32::MAX as u64) as u32;
        let base_effect = BaseEffect {
            kind: BaseEffectType::Strong { magnitude },
            scheduling: Replay {
                play_for: Ticks::from_ms(duration_ms),
                ..Default::default()
            },
            ..Default::default()
        };

        let effect = EffectBuilder::new().add_effect(base_effect).gamepads(&supported).finish(&mut self.gilrs);

        match effect {
            Ok(effect) => {
                if let Err(err) = effect.play() {
                    tracing::warn!("Failed to play gamepad vibration: {err}");
                    return;
                }

                let expires_at = Instant::now() + Duration::from_millis(duration_ms as u64 + 250);
                self.active_effects.push((expires_at, effect));
            }
            Err(err) => {
                tracing::warn!("Failed to build gamepad vibration effect: {err}");
            }
        }
    }
}

fn stick_axis(axis: Axis) -> Option<(StickKind, StickAxisComponent)> {
    match axis {
        Axis::LeftStickX => Some((StickKind::Left, StickAxisComponent::X)),
        Axis::LeftStickY => Some((StickKind::Left, StickAxisComponent::Y)),
        Axis::RightStickX => Some((StickKind::Right, StickAxisComponent::X)),
        Axis::RightStickY => Some((StickKind::Right, StickAxisComponent::Y)),
        _ => None,
    }
}

fn resolve_stick_direction(stick: StickKind, x: f32, y: f32, current_direction: Option<GamepadAxisDirection>) -> Option<GamepadAxisDirection> {
    let magnitude = x.hypot(y);

    let activation_threshold = if current_direction.is_some() {
        AXIS_RELEASE_THRESHOLD
    } else {
        AXIS_PRESS_THRESHOLD
    };

    if magnitude < activation_threshold {
        return None;
    }

    if x.abs() >= y.abs() {
        if x >= 0.0 {
            Some(match stick {
                StickKind::Left => GamepadAxisDirection::LeftStickRight,
                StickKind::Right => GamepadAxisDirection::RightStickRight,
            })
        } else {
            Some(match stick {
                StickKind::Left => GamepadAxisDirection::LeftStickLeft,
                StickKind::Right => GamepadAxisDirection::RightStickLeft,
            })
        }
    } else if y >= 0.0 {
        Some(match stick {
            StickKind::Left => GamepadAxisDirection::LeftStickDown,
            StickKind::Right => GamepadAxisDirection::RightStickDown,
        })
    } else {
        Some(match stick {
            StickKind::Left => GamepadAxisDirection::LeftStickUp,
            StickKind::Right => GamepadAxisDirection::RightStickUp,
        })
    }
}
