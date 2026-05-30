use std::time::Duration;

#[cfg(windows)]
use windows_sys::Win32::{
    System::SystemInformation::GetTickCount64,
    UI::Input::KeyboardAndMouse::{GetAsyncKeyState, GetLastInputInfo, LASTINPUTINFO, VK_LBUTTON},
    UI::WindowsAndMessaging::{ShowWindowAsync, SW_HIDE, SW_SHOWNOACTIVATE},
};

#[cfg(windows)]
pub fn last_input_tick() -> Option<u32> {
    let mut info = LASTINPUTINFO {
        cbSize: std::mem::size_of::<LASTINPUTINFO>() as u32,
        dwTime: 0,
    };
    let ok = unsafe { GetLastInputInfo(&mut info) };
    (ok != 0).then_some(info.dwTime)
}

#[cfg(not(windows))]
pub fn last_input_tick() -> Option<u32> {
    None
}

#[cfg(windows)]
pub fn user_idle_duration() -> Option<Duration> {
    let last = last_input_tick()?;
    let now = unsafe { GetTickCount64() } as u32;
    Some(Duration::from_millis(now.wrapping_sub(last) as u64))
}

#[cfg(not(windows))]
pub fn user_idle_duration() -> Option<Duration> {
    None
}

#[cfg(windows)]
pub fn is_left_mouse_down() -> bool {
    unsafe { GetAsyncKeyState(VK_LBUTTON as i32) & i16::MIN != 0 }
}

#[cfg(not(windows))]
pub fn is_left_mouse_down() -> bool {
    false
}

#[cfg(windows)]
pub fn show_window(hwnd: isize, visible: bool) {
    let cmd = if visible { SW_SHOWNOACTIVATE } else { SW_HIDE };
    unsafe {
        ShowWindowAsync(hwnd as *mut core::ffi::c_void, cmd);
    }
}

#[cfg(not(windows))]
pub fn show_window(_hwnd: isize, _visible: bool) {}

pub fn frame_hwnd(_frame: &eframe::Frame) -> Option<isize> {
    None
}
