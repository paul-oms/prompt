//! Mapping winit key events onto Ghostty's key encoder.

use winit::keyboard::KeyCode;

use crate::ffi;
use crate::term::TerminalSession;

pub fn is_macos() -> bool {
    cfg!(target_os = "macos")
}

pub fn ghostty_key(code: KeyCode) -> Option<ffi::GhosttyKey> {
    use KeyCode::*;
    Some(match code {
        KeyA => ffi::GHOSTTY_KEY_A,
        KeyB => ffi::GHOSTTY_KEY_B,
        KeyC => ffi::GHOSTTY_KEY_C,
        KeyD => ffi::GHOSTTY_KEY_D,
        KeyE => ffi::GHOSTTY_KEY_E,
        KeyF => ffi::GHOSTTY_KEY_F,
        KeyG => ffi::GHOSTTY_KEY_G,
        KeyH => ffi::GHOSTTY_KEY_H,
        KeyI => ffi::GHOSTTY_KEY_I,
        KeyJ => ffi::GHOSTTY_KEY_J,
        KeyK => ffi::GHOSTTY_KEY_K,
        KeyL => ffi::GHOSTTY_KEY_L,
        KeyM => ffi::GHOSTTY_KEY_M,
        KeyN => ffi::GHOSTTY_KEY_N,
        KeyO => ffi::GHOSTTY_KEY_O,
        KeyP => ffi::GHOSTTY_KEY_P,
        KeyQ => ffi::GHOSTTY_KEY_Q,
        KeyR => ffi::GHOSTTY_KEY_R,
        KeyS => ffi::GHOSTTY_KEY_S,
        KeyT => ffi::GHOSTTY_KEY_T,
        KeyU => ffi::GHOSTTY_KEY_U,
        KeyV => ffi::GHOSTTY_KEY_V,
        KeyW => ffi::GHOSTTY_KEY_W,
        KeyX => ffi::GHOSTTY_KEY_X,
        KeyY => ffi::GHOSTTY_KEY_Y,
        KeyZ => ffi::GHOSTTY_KEY_Z,
        Digit0 => ffi::GHOSTTY_KEY_DIGIT_0,
        Digit1 => ffi::GHOSTTY_KEY_DIGIT_1,
        Digit2 => ffi::GHOSTTY_KEY_DIGIT_2,
        Digit3 => ffi::GHOSTTY_KEY_DIGIT_3,
        Digit4 => ffi::GHOSTTY_KEY_DIGIT_4,
        Digit5 => ffi::GHOSTTY_KEY_DIGIT_5,
        Digit6 => ffi::GHOSTTY_KEY_DIGIT_6,
        Digit7 => ffi::GHOSTTY_KEY_DIGIT_7,
        Digit8 => ffi::GHOSTTY_KEY_DIGIT_8,
        Digit9 => ffi::GHOSTTY_KEY_DIGIT_9,
        F1 => ffi::GHOSTTY_KEY_F1,
        F2 => ffi::GHOSTTY_KEY_F2,
        F3 => ffi::GHOSTTY_KEY_F3,
        F4 => ffi::GHOSTTY_KEY_F4,
        F5 => ffi::GHOSTTY_KEY_F5,
        F6 => ffi::GHOSTTY_KEY_F6,
        F7 => ffi::GHOSTTY_KEY_F7,
        F8 => ffi::GHOSTTY_KEY_F8,
        F9 => ffi::GHOSTTY_KEY_F9,
        F10 => ffi::GHOSTTY_KEY_F10,
        F11 => ffi::GHOSTTY_KEY_F11,
        F12 => ffi::GHOSTTY_KEY_F12,
        F13 => ffi::GHOSTTY_KEY_F13,
        F14 => ffi::GHOSTTY_KEY_F14,
        F15 => ffi::GHOSTTY_KEY_F15,
        F16 => ffi::GHOSTTY_KEY_F16,
        F17 => ffi::GHOSTTY_KEY_F17,
        F18 => ffi::GHOSTTY_KEY_F18,
        F19 => ffi::GHOSTTY_KEY_F19,
        F20 => ffi::GHOSTTY_KEY_F20,
        Escape => ffi::GHOSTTY_KEY_ESCAPE,
        Enter => ffi::GHOSTTY_KEY_ENTER,
        Tab => ffi::GHOSTTY_KEY_TAB,
        Space => ffi::GHOSTTY_KEY_SPACE,
        Backspace => ffi::GHOSTTY_KEY_BACKSPACE,
        Delete => ffi::GHOSTTY_KEY_DELETE,
        Insert => ffi::GHOSTTY_KEY_INSERT,
        Home => ffi::GHOSTTY_KEY_HOME,
        End => ffi::GHOSTTY_KEY_END,
        PageUp => ffi::GHOSTTY_KEY_PAGE_UP,
        PageDown => ffi::GHOSTTY_KEY_PAGE_DOWN,
        ArrowUp => ffi::GHOSTTY_KEY_ARROW_UP,
        ArrowDown => ffi::GHOSTTY_KEY_ARROW_DOWN,
        ArrowLeft => ffi::GHOSTTY_KEY_ARROW_LEFT,
        ArrowRight => ffi::GHOSTTY_KEY_ARROW_RIGHT,
        Backquote => ffi::GHOSTTY_KEY_BACKQUOTE,
        Backslash => ffi::GHOSTTY_KEY_BACKSLASH,
        BracketLeft => ffi::GHOSTTY_KEY_BRACKET_LEFT,
        BracketRight => ffi::GHOSTTY_KEY_BRACKET_RIGHT,
        Comma => ffi::GHOSTTY_KEY_COMMA,
        Equal => ffi::GHOSTTY_KEY_EQUAL,
        Minus => ffi::GHOSTTY_KEY_MINUS,
        Period => ffi::GHOSTTY_KEY_PERIOD,
        Quote => ffi::GHOSTTY_KEY_QUOTE,
        Semicolon => ffi::GHOSTTY_KEY_SEMICOLON,
        Slash => ffi::GHOSTTY_KEY_SLASH,
        NumpadAdd => ffi::GHOSTTY_KEY_NUMPAD_ADD,
        NumpadSubtract => ffi::GHOSTTY_KEY_NUMPAD_SUBTRACT,
        NumpadMultiply => ffi::GHOSTTY_KEY_NUMPAD_MULTIPLY,
        NumpadDivide => ffi::GHOSTTY_KEY_NUMPAD_DIVIDE,
        NumpadEnter => ffi::GHOSTTY_KEY_NUMPAD_ENTER,
        Numpad0 => ffi::GHOSTTY_KEY_NUMPAD_0,
        Numpad1 => ffi::GHOSTTY_KEY_NUMPAD_1,
        Numpad2 => ffi::GHOSTTY_KEY_NUMPAD_2,
        Numpad3 => ffi::GHOSTTY_KEY_NUMPAD_3,
        Numpad4 => ffi::GHOSTTY_KEY_NUMPAD_4,
        Numpad5 => ffi::GHOSTTY_KEY_NUMPAD_5,
        Numpad6 => ffi::GHOSTTY_KEY_NUMPAD_6,
        Numpad7 => ffi::GHOSTTY_KEY_NUMPAD_7,
        Numpad8 => ffi::GHOSTTY_KEY_NUMPAD_8,
        Numpad9 => ffi::GHOSTTY_KEY_NUMPAD_9,
        _ => return None,
    })
}

pub fn ghostty_mods(mods: winit::keyboard::ModifiersState) -> ffi::GhosttyMods {
    let mut out = 0u16;
    if mods.shift_key() {
        out |= ffi::GHOSTTY_MODS_SHIFT as u16;
    }
    if mods.control_key() {
        out |= ffi::GHOSTTY_MODS_CTRL as u16;
    }
    if mods.alt_key() {
        out |= ffi::GHOSTTY_MODS_ALT as u16;
    }
    if mods.super_key() {
        out |= ffi::GHOSTTY_MODS_SUPER as u16;
    }
    out
}

/// Encode a key press for a program running in the terminal.
pub fn encode(
    term: &mut TerminalSession,
    code: KeyCode,
    mods: winit::keyboard::ModifiersState,
    text: Option<&str>,
) -> Vec<u8> {
    let key = ghostty_key(code).unwrap_or(ffi::GHOSTTY_KEY_UNIDENTIFIED);
    term.encode_key(key, ghostty_mods(mods), ffi::GHOSTTY_KEY_ACTION_PRESS, text)
}
