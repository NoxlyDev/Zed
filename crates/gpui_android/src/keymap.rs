//! Android keycodes -> GPUI keystrokes.

use android_activity::input::{KeyEvent, Keycode, MetaState};
use gpui::{Keystroke, Modifiers};

pub(crate) fn modifiers(meta: MetaState) -> Modifiers {
    Modifiers {
        control: meta.ctrl_on(),
        alt: meta.alt_on(),
        shift: meta.shift_on(),
        platform: meta.meta_on(),
        function: meta.function_on(),
    }
}

/// (key name, unshifted char, shifted char)
fn lookup(code: Keycode) -> Option<(String, Option<char>, Option<char>)> {
    use Keycode::*;
    macro_rules! letter {
        ($($v:ident $l:literal $u:literal),*) => {
            match code { $($v => return Some(($l.to_string(), Some($l), Some($u))),)* _ => {} }
        };
    }
    letter!(A 'a' 'A', B 'b' 'B', C 'c' 'C', D 'd' 'D', E 'e' 'E', F 'f' 'F', G 'g' 'G',
            H 'h' 'H', I 'i' 'I', J 'j' 'J', K 'k' 'K', L 'l' 'L', M 'm' 'M', N 'n' 'N',
            O 'o' 'O', P 'p' 'P', Q 'q' 'Q', R 'r' 'R', S 's' 'S', T 't' 'T', U 'u' 'U',
            V 'v' 'V', W 'w' 'W', X 'x' 'X', Y 'y' 'Y', Z 'z' 'Z');
    let (key, plain, shifted) = match code {
        Keycode0 => ("0", Some('0'), Some(')')),
        Keycode1 => ("1", Some('1'), Some('!')),
        Keycode2 => ("2", Some('2'), Some('@')),
        Keycode3 => ("3", Some('3'), Some('#')),
        Keycode4 => ("4", Some('4'), Some('$')),
        Keycode5 => ("5", Some('5'), Some('%')),
        Keycode6 => ("6", Some('6'), Some('^')),
        Keycode7 => ("7", Some('7'), Some('&')),
        Keycode8 => ("8", Some('8'), Some('*')),
        Keycode9 => ("9", Some('9'), Some('(')),
        Comma => (",", Some(','), Some('<')),
        Period => (".", Some('.'), Some('>')),
        Minus => ("-", Some('-'), Some('_')),
        Equals => ("=", Some('='), Some('+')),
        LeftBracket => ("[", Some('['), Some('{')),
        RightBracket => ("]", Some(']'), Some('}')),
        Backslash => ("\\", Some('\\'), Some('|')),
        Semicolon => (";", Some(';'), Some(':')),
        Apostrophe => ("'", Some('\''), Some('"')),
        Slash => ("/", Some('/'), Some('?')),
        Grave => ("`", Some('`'), Some('~')),
        Space => ("space", Some(' '), Some(' ')),
        Enter | NumpadEnter => ("enter", None, None),
        Del => ("backspace", None, None),
        ForwardDel => ("delete", None, None),
        Tab => ("tab", None, None),
        Escape => ("escape", None, None),
        DpadUp => ("up", None, None),
        DpadDown => ("down", None, None),
        DpadLeft => ("left", None, None),
        DpadRight => ("right", None, None),
        MoveHome => ("home", None, None),
        MoveEnd => ("end", None, None),
        PageUp => ("pageup", None, None),
        PageDown => ("pagedown", None, None),
        Insert => ("insert", None, None),
        F1 => ("f1", None, None),
        F2 => ("f2", None, None),
        F3 => ("f3", None, None),
        F4 => ("f4", None, None),
        F5 => ("f5", None, None),
        F6 => ("f6", None, None),
        F7 => ("f7", None, None),
        F8 => ("f8", None, None),
        F9 => ("f9", None, None),
        F10 => ("f10", None, None),
        F11 => ("f11", None, None),
        F12 => ("f12", None, None),
        _ => return None,
    };
    Some((key.to_string(), plain, shifted))
}

/// Returns `None` for keys GPUI has no name for (modifiers, media keys, Back).
pub(crate) fn keystroke(event: &KeyEvent<'_>) -> Option<Keystroke> {
    let (key, plain, shifted) = lookup(event.key_code())?;
    let modifiers = modifiers(event.meta_state());
    let typing = !modifiers.control && !modifiers.alt && !modifiers.platform;
    let key_char = if typing {
        let ch = if modifiers.shift { shifted } else { plain };
        ch.map(|c| c.to_string())
    } else {
        None
    };
    Some(Keystroke { modifiers, key, key_char })
}
