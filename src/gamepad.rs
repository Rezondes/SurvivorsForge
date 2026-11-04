//! A game controller as keys, for the Steam Deck in Game Mode and couch play: the D-pad moves
//! the focus, A presses, B goes back, the shoulder buttons switch pages. The window then handles
//! them like the keyboard. Linux reads the devices through evdev (no system library to build
//! against), Windows and macOS through gilrs.

/// A button the launcher listens to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pad {
    Up,
    Down,
    Left,
    Right,
    /// A (Xbox), cross (PlayStation).
    South,
    /// B, circle.
    East,
    LeftShoulder,
    RightShoulder,
}

/// What a button stands for on the keyboard.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Press {
    /// Tab, or Shift+Tab with `back`.
    Focus { back: bool },
    Key(slint::platform::Key),
}

pub fn press(pad: Pad) -> Press {
    use slint::platform::Key;
    match pad {
        Pad::Up => Press::Focus { back: true },
        Pad::Down => Press::Focus { back: false },
        // Left and right stay arrows: they step through pictures and change a choice.
        Pad::Left => Press::Key(Key::LeftArrow),
        Pad::Right => Press::Key(Key::RightArrow),
        Pad::South => Press::Key(Key::Return),
        Pad::East => Press::Key(Key::Escape),
        Pad::LeftShoulder => Press::Key(Key::PageUp),
        Pad::RightShoulder => Press::Key(Key::PageDown),
    }
}

/// The D-pad of many controllers is a hat axis: -1, 0 or 1 on X (left, right) and Y (up, down).
pub fn hat(vertical: bool, value: i32) -> Option<Pad> {
    match (vertical, value.signum()) {
        (false, -1) => Some(Pad::Left),
        (false, 1) => Some(Pad::Right),
        (true, -1) => Some(Pad::Up),
        (true, 1) => Some(Pad::Down),
        _ => None,
    }
}

/// Calls `on_press` for every button pressed on any controller, on threads of its own, for as
/// long as the app runs. Without a controller (or without access to one) it does nothing.
pub fn listen(on_press: impl Fn(Pad) + Send + Sync + 'static) {
    backend::listen(std::sync::Arc::new(on_press));
}

#[cfg(target_os = "linux")]
mod backend {
    use super::{Pad, hat};
    use evdev::{AbsoluteAxisCode, EventSummary, KeyCode};
    use std::collections::HashSet;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    fn button(code: KeyCode) -> Option<Pad> {
        Some(match code {
            KeyCode::BTN_SOUTH => Pad::South,
            KeyCode::BTN_EAST => Pad::East,
            KeyCode::BTN_TL => Pad::LeftShoulder,
            KeyCode::BTN_TR => Pad::RightShoulder,
            KeyCode::BTN_DPAD_UP => Pad::Up,
            KeyCode::BTN_DPAD_DOWN => Pad::Down,
            KeyCode::BTN_DPAD_LEFT => Pad::Left,
            KeyCode::BTN_DPAD_RIGHT => Pad::Right,
            _ => return None,
        })
    }

    pub fn listen(on_press: Arc<dyn Fn(Pad) + Send + Sync>) {
        let watched = Arc::new(Mutex::new(HashSet::new()));
        // ponytail: looks for new controllers every few seconds; udev events if that feels slow
        std::thread::spawn(move || loop {
            for (path, device) in evdev::enumerate() {
                let is_pad = device.supported_keys().is_some_and(|k| k.contains(KeyCode::BTN_SOUTH));
                if !is_pad || !watched.lock().unwrap_or_else(|e| e.into_inner()).insert(path.clone()) {
                    continue;
                }
                let (on_press, watched) = (on_press.clone(), watched.clone());
                std::thread::spawn(move || {
                    let mut device = device;
                    while let Ok(events) = device.fetch_events() {
                        for event in events {
                            let pad = match event.destructure() {
                                EventSummary::Key(_, code, 1) => button(code),
                                EventSummary::AbsoluteAxis(_, AbsoluteAxisCode::ABS_HAT0X, v) => hat(false, v),
                                EventSummary::AbsoluteAxis(_, AbsoluteAxisCode::ABS_HAT0Y, v) => hat(true, v),
                                _ => None,
                            };
                            if let Some(pad) = pad {
                                on_press(pad);
                            }
                        }
                    }
                    // Unplugged: it may come back under the same path.
                    watched.lock().unwrap_or_else(|e| e.into_inner()).remove(&path);
                });
            }
            std::thread::sleep(Duration::from_secs(3));
        });
    }
}

#[cfg(not(target_os = "linux"))]
mod backend {
    use super::Pad;
    use gilrs::{Button, EventType, Gilrs};
    use std::sync::Arc;

    pub fn listen(on_press: Arc<dyn Fn(Pad) + Send + Sync>) {
        std::thread::spawn(move || {
            let Ok(mut gilrs) = Gilrs::new() else { return };
            loop {
                while let Some(event) = gilrs.next_event_blocking(None) {
                    let EventType::ButtonPressed(button, _) = event.event else { continue };
                    let pad = match button {
                        Button::South => Pad::South,
                        Button::East => Pad::East,
                        Button::LeftTrigger => Pad::LeftShoulder,
                        Button::RightTrigger => Pad::RightShoulder,
                        Button::DPadUp => Pad::Up,
                        Button::DPadDown => Pad::Down,
                        Button::DPadLeft => Pad::Left,
                        Button::DPadRight => Pad::Right,
                        _ => continue,
                    };
                    on_press(pad);
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use slint::platform::Key;

    #[test]
    fn buttons_stand_for_the_keys_the_window_knows() {
        assert_eq!(press(Pad::Down), Press::Focus { back: false });
        assert_eq!(press(Pad::Up), Press::Focus { back: true });
        assert_eq!(press(Pad::South), Press::Key(Key::Return));
        assert_eq!(press(Pad::East), Press::Key(Key::Escape), "B goes back");
        assert_eq!(press(Pad::RightShoulder), Press::Key(Key::PageDown));
    }

    #[test]
    fn a_hat_d_pad_turns_into_directions() {
        assert_eq!(hat(true, -1), Some(Pad::Up));
        assert_eq!(hat(false, 1), Some(Pad::Right));
        assert_eq!(hat(true, 0), None, "let go");
    }
}
