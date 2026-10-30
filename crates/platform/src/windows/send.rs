//! Building and sending keyboard input. Every event carries [`INJECTED`] so
//! the backend's own hook lets it through untouched.

use windows::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_0, INPUT_KEYBOARD, KEYBD_EVENT_FLAGS, KEYBDINPUT, KEYEVENTF_KEYUP,
    KEYEVENTF_UNICODE, SendInput, VIRTUAL_KEY, VK_RETURN, VK_SHIFT,
};

use super::keys::VK_MASK;
use super::text::{Stroke, strokes};

/// `dwExtraInfo` of input the backend injects ("oftp").
pub const INJECTED: usize = 0x6f66_7470;

fn input(vk: VIRTUAL_KEY, scan: u16, flags: KEYBD_EVENT_FLAGS) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: vk,
                wScan: scan,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: INJECTED,
            },
        },
    }
}

/// A virtual key press and release.
pub fn tap(out: &mut Vec<INPUT>, vk: VIRTUAL_KEY) {
    out.push(input(vk, 0, KEYBD_EVENT_FLAGS(0)));
    out.push(input(vk, 0, KEYEVENTF_KEYUP));
}

pub fn press(out: &mut Vec<INPUT>, vk: VIRTUAL_KEY, down: bool) {
    out.push(input(
        vk,
        0,
        if down {
            KEYBD_EVENT_FLAGS(0)
        } else {
            KEYEVENTF_KEYUP
        },
    ));
}

/// Keystrokes that type `text` as Unicode characters.
pub fn text(out: &mut Vec<INPUT>, text: &str) {
    for s in strokes(text) {
        match s {
            Stroke::Unit(u) => {
                out.push(input(VIRTUAL_KEY(0), u, KEYEVENTF_UNICODE));
                out.push(input(
                    VIRTUAL_KEY(0),
                    u,
                    KEYEVENTF_UNICODE | KEYEVENTF_KEYUP,
                ));
            }
            Stroke::NewLine => {
                press(out, VK_SHIFT, true);
                tap(out, VK_RETURN);
                press(out, VK_SHIFT, false);
            }
        }
    }
}

/// An unassigned key, so that releasing a held Alt or Win after the gate
/// swallowed a key does not open the menu bar or the Start menu.
pub fn mask(out: &mut Vec<INPUT>) {
    tap(out, VIRTUAL_KEY(u16::from(VK_MASK)));
}

/// Sends `inputs` as one uninterruptible batch. Returns false when Windows
/// rejected some of them (another desktop, or a window of higher integrity).
pub fn send(inputs: &[INPUT]) -> bool {
    if inputs.is_empty() {
        return true;
    }
    // SAFETY: every element is a fully initialised keyboard INPUT and the size
    // is that of the struct.
    let sent = unsafe { SendInput(inputs, size_of::<INPUT>() as i32) };
    sent as usize == inputs.len()
}
