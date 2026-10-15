// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

// Copyright 2026 Oxide Computer Company

//! Giving a Windows GUI binary somewhere to print.
//!
//! A PE image declares its subsystem at link time and cannot change it at
//! runtime. `oxwin.exe` is linked as a GUI binary, because that is the only way a
//! double-click does not flash a console window — and the person clicking it is
//! the reason this binary exists. The cost is that it starts with no standard
//! handles at all, so on the CLI route `println!` would write into nothing.
//!
//! [`attach`] fixes that: it joins the console of whatever launched it, opens
//! `CONOUT$`/`CONIN$` on that console, and installs those as the standard handles
//! that were missing. Rust's std re-reads the process's standard handles on each
//! write, so this is enough; no CRT `freopen` is involved.
//!
//! What it cannot fix is that a shell does not wait for a GUI-subsystem process.
//! `cmd` and PowerShell return their prompt immediately while output is still
//! arriving, so an ordering-sensitive script needs `start /wait` or `| Out-Host`.
//! A small console-subsystem shim that re-execs this binary is the standard fix
//! if that becomes a real problem; nothing here would have to change.

/// Attach whichever standard streams are missing to a usable console.
///
/// Called on the CLI route only, before anything prints. Best effort throughout:
/// a failure here means output goes nowhere, which must not stop a build that was
/// going to work.
///
/// **Only missing handles are replaced.** `oxwin build … > log.txt` from a shell
/// has a perfectly good stdout — the file — *and* an attachable parent console, so
/// overwriting the handles unconditionally would quietly break every redirect and
/// every pipe.
#[cfg(windows)]
pub fn attach() {
    use windows_sys::Win32::Foundation::{HANDLE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::CreateFileW;
    use windows_sys::Win32::System::Console::{
        ATTACH_PARENT_PROCESS, AllocConsole, AttachConsole, GetStdHandle,
        STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, SetStdHandle,
    };

    // Spelled out rather than imported: these few constants have moved between
    // modules across windows-sys releases, and their values have not moved since
    // Windows NT.
    const GENERIC_READ: u32 = 0x8000_0000;
    const GENERIC_WRITE: u32 = 0x4000_0000;
    const FILE_SHARE_READ: u32 = 0x0000_0001;
    const FILE_SHARE_WRITE: u32 = 0x0000_0002;
    const OPEN_EXISTING: u32 = 3;

    let present = |which: u32| -> bool {
        let handle = unsafe { GetStdHandle(which) };
        !handle.is_null() && handle != INVALID_HANDLE_VALUE
    };
    let missing: Vec<u32> =
        [STD_OUTPUT_HANDLE, STD_ERROR_HANDLE, STD_INPUT_HANDLE]
            .into_iter()
            .filter(|which| !present(*which))
            .collect();
    if missing.is_empty() {
        return;
    }

    if unsafe { AttachConsole(ATTACH_PARENT_PROCESS) } == 0 {
        // No parent console: `oxwin.exe doctor` double-clicked, or launched from
        // something with no terminal at all. A console of its own is better than
        // output that goes nowhere, even though it closes on exit.
        unsafe { AllocConsole() };
    }

    let open = |name: &str| -> Option<HANDLE> {
        let wide: Vec<u16> =
            name.encode_utf16().chain(std::iter::once(0)).collect();
        // Opened read-write in both directions: a console handle is bidirectional,
        // and CONOUT$ refuses a read-only open on some Windows versions.
        let handle = unsafe {
            CreateFileW(
                wide.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                std::ptr::null(),
                OPEN_EXISTING,
                0,
                std::ptr::null_mut(),
            )
        };
        (!handle.is_null() && handle != INVALID_HANDLE_VALUE).then_some(handle)
    };

    // One open per direction, shared between stdout and stderr: they address the
    // same console, `SetStdHandle` does not take ownership, and two opens of
    // CONOUT$ would interleave no better than one.
    let out = open("CONOUT$");
    let input = open("CONIN$");
    for which in missing {
        let handle = if which == STD_INPUT_HANDLE { input } else { out };
        if let Some(handle) = handle {
            unsafe { SetStdHandle(which, handle) };
        }
    }
}

/// Nothing to do anywhere else: a Unix process is handed its standard streams by
/// whatever started it.
#[cfg(not(windows))]
pub fn attach() {}
