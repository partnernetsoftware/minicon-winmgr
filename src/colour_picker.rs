//! One colour chooser for every colour setting: desktop colour and theme accent.
//!
//! Modal and non-reentrant. The owner window is passed in so the system dialog stays
//! attached to the settings surface that opened it. Colours use the `0x00BBGGRR`
//! layout the theme uses; the custom slot holds only the current value.
use std::cell::Cell;
use windows::Win32::Foundation::{COLORREF, HWND};
use windows::Win32::UI::Controls::Dialogs::{CC_FULLOPEN, CC_RGBINIT, CHOOSECOLORW, ChooseColorW};

thread_local! { static OPEN: Cell<bool> = const { Cell::new(false) }; }

/// Opens the system colour chooser starting at `initial`. Returns the chosen colour, or
/// `None` when cancelled or when a chooser is already open.
pub(crate) fn pick(owner: HWND, initial: u32) -> Option<u32> {
    if OPEN.with(|open| open.replace(true)) {
        return None;
    }
    let mut custom = [COLORREF(0); 16];
    custom[0] = COLORREF(initial);
    let mut chooser = CHOOSECOLORW {
        lStructSize: std::mem::size_of::<CHOOSECOLORW>() as u32,
        hwndOwner: owner,
        rgbResult: COLORREF(initial),
        lpCustColors: custom.as_mut_ptr(),
        Flags: CC_RGBINIT | CC_FULLOPEN,
        ..Default::default()
    };
    // SAFETY: the structure and the custom-colour array live across the call.
    let chosen = unsafe { ChooseColorW(&mut chooser) }.as_bool();
    OPEN.with(|open| open.set(false));
    chosen.then_some(chooser.rgbResult.0 & 0x00ff_ffff)
}
