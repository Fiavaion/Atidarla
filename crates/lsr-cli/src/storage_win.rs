//! The Windows side of `storage uninstall` ([`super`]): the timed question,
//! whether anyone can see it, and whether this process is the user signed in
//! to its session.

use super::{ANSWER_WITHIN, Dialog};

/// A Yes/No message box that defaults to No (keep), stays on top of the
/// uninstaller's progress window and gives up after [`ANSWER_WITHIN`]. Not
/// shown in session 0 or on a window station nobody can see.
///
/// The timeout is Microsoft's documented timed message box (KB 181934): a
/// thread timer posts `WM_QUIT`, which ends the box's modal loop; a `WM_QUIT`
/// left in the queue afterwards means the timer fired. `MessageBoxTimeoutW`
/// is undocumented and not in `windows-sys`, and `TaskDialogIndirect` needs
/// common controls 6, which a console program without a manifest does not
/// load (its import would stop the program from starting).
pub(super) fn dialog(text: &str) -> Option<Dialog> {
    use windows_sys::Win32::Foundation::HWND;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        IDNO, IDYES, KillTimer, MB_DEFBUTTON2, MB_ICONQUESTION, MB_SETFOREGROUND, MB_TOPMOST,
        MB_YESNO, MSG, MessageBoxW, PM_REMOVE, PeekMessageW, PostQuitMessage, SetTimer, WM_QUIT,
    };
    if !someone_can_see() {
        return Some(Dialog::Unattended);
    }
    unsafe extern "system" fn time_up(_: HWND, _: u32, _: usize, _: u32) {
        // SAFETY: posts to this thread's own queue.
        unsafe { PostQuitMessage(0) };
    }
    let wide = |s: &str| s.encode_utf16().chain([0]).collect::<Vec<u16>>();
    let (text, title) = (wide(text), wide("Uninstall Lasair"));
    let ms = u32::try_from(ANSWER_WITHIN.as_millis()).unwrap_or(u32::MAX);
    // SAFETY: a thread timer (no window) whose callback only posts WM_QUIT;
    // both strings are NUL-terminated UTF-16 that outlive the call; a null
    // owner window is allowed; `msg` is a valid MSG to fill.
    let (answer, timed_out) = unsafe {
        let timer = SetTimer(std::ptr::null_mut(), 0, ms, Some(time_up));
        let answer = MessageBoxW(
            std::ptr::null_mut(),
            text.as_ptr(),
            title.as_ptr(),
            MB_YESNO | MB_ICONQUESTION | MB_DEFBUTTON2 | MB_TOPMOST | MB_SETFOREGROUND,
        );
        if timer != 0 {
            KillTimer(std::ptr::null_mut(), timer);
        }
        let mut msg: MSG = std::mem::zeroed();
        let quit = PeekMessageW(&mut msg, std::ptr::null_mut(), WM_QUIT, WM_QUIT, PM_REMOVE) != 0;
        (answer, quit)
    };
    Some(match answer {
        _ if timed_out => Dialog::TimedOut,
        IDYES => Dialog::Yes,
        IDNO => Dialog::No,
        _ => Dialog::Failed,
    })
}

/// Whether a dialog from this process can be seen: not in session 0
/// (services, and installs run by a deployment tool as SYSTEM), and only on
/// a visible window station.
fn someone_can_see() -> bool {
    use windows_sys::Win32::System::RemoteDesktop::ProcessIdToSessionId;
    use windows_sys::Win32::System::StationsAndDesktops::{
        GetProcessWindowStation, GetUserObjectInformationW, UOI_FLAGS, USEROBJECTFLAGS,
    };
    use windows_sys::Win32::System::Threading::GetCurrentProcessId;
    use windows_sys::Win32::UI::WindowsAndMessaging::WSF_VISIBLE;
    let mut session = 0u32;
    // SAFETY: `session` is a valid u32 to fill.
    if unsafe { ProcessIdToSessionId(GetCurrentProcessId(), &mut session) } == 0 || session == 0 {
        return false;
    }
    let mut flags = USEROBJECTFLAGS::default();
    let mut needed = 0u32;
    // SAFETY: the process's window station handle is not ours to close;
    // `flags` is a USEROBJECTFLAGS of the size passed.
    let ok = unsafe {
        GetUserObjectInformationW(
            GetProcessWindowStation(),
            UOI_FLAGS,
            (&raw mut flags).cast(),
            std::mem::size_of::<USEROBJECTFLAGS>() as u32,
            &mut needed,
        )
    };
    ok != 0 && flags.dwFlags & WSF_VISIBLE as u32 != 0
}

/// Why this process must not act for the person signed in to its session:
/// it runs as another account (an elevation prompt answered with an
/// administrator's name and password runs the uninstall as that account),
/// nobody is signed in, or who is cannot be told.
pub(super) fn other_user() -> Option<String> {
    match (process_user(), session_user()) {
        // Windows account names are not case-sensitive.
        (Some(p), Some(s)) if p.to_lowercase() == s.to_lowercase() => None,
        (Some(p), Some(s)) if s.is_empty() => Some(format!(
            "running as {p} with no one signed in to this session"
        )),
        (Some(p), Some(s)) => Some(format!(
            "running as {p}, not as {s} who is signed in, so this account's folders are not {s}'s"
        )),
        _ => Some("could not tell who is signed in to this session".to_owned()),
    }
}

/// The account this process runs as.
fn process_user() -> Option<String> {
    use windows_sys::Win32::System::WindowsProgramming::GetUserNameW;
    let mut buf = [0u16; 257];
    let mut len = buf.len() as u32;
    // SAFETY: `buf` holds `len` UTF-16 units.
    if unsafe { GetUserNameW(buf.as_mut_ptr(), &mut len) } == 0 {
        return None;
    }
    // `len` counts the terminating NUL.
    Some(String::from_utf16_lossy(
        &buf[..(len as usize).saturating_sub(1)],
    ))
}

/// The account signed in to this process's session (empty when none is).
fn session_user() -> Option<String> {
    use windows_sys::Win32::System::RemoteDesktop::{
        WTS_CURRENT_SERVER_HANDLE, WTS_CURRENT_SESSION, WTSFreeMemory, WTSQuerySessionInformationW,
        WTSUserName,
    };
    let mut ptr: windows_sys::core::PWSTR = std::ptr::null_mut();
    let mut bytes = 0u32;
    // SAFETY: on success `ptr` is a NUL-terminated UTF-16 string of `bytes`
    // bytes owned by WTS, read and then freed here.
    unsafe {
        if WTSQuerySessionInformationW(
            WTS_CURRENT_SERVER_HANDLE,
            WTS_CURRENT_SESSION,
            WTSUserName,
            &mut ptr,
            &mut bytes,
        ) == 0
            || ptr.is_null()
        {
            return None;
        }
        let units = std::slice::from_raw_parts(ptr, bytes as usize / 2);
        let end = units.iter().position(|&u| u == 0).unwrap_or(units.len());
        let name = String::from_utf16_lossy(&units[..end]);
        WTSFreeMemory(ptr.cast());
        Some(name)
    }
}
