//! The app's identity on Windows: an AppUserModelID for this process and a
//! Start Menu shortcut carrying it. Windows shows a toast under the name and
//! icon of the shortcut whose AppUserModelID it's sent with; without one,
//! toasts don't show at all, so they're only sent with ours once the
//! shortcut exists (see [`toast_app_id`]).

use std::path::PathBuf;

use anyhow::{Context, Result};
use windows::Win32::Storage::EnhancedStorage::PKEY_AppUserModel_ID;
use windows::Win32::System::Com::StructuredStorage::PROPVARIANT;
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoUninitialize, IPersistFile,
};
use windows::Win32::UI::Shell::PropertiesSystem::IPropertyStore;
use windows::Win32::UI::Shell::{IShellLinkW, SetCurrentProcessExplicitAppUserModelID, ShellLink};
use windows::core::{HSTRING, Interface};

pub const AUMID: &str = "io.github.mostaxd.Mostalgia";

/// `%APPDATA%\Microsoft\Windows\Start Menu\Programs\Mostalgia.lnk`
pub fn shortcut_path() -> Option<PathBuf> {
    let appdata = std::env::var_os("APPDATA")?;
    Some(PathBuf::from(appdata).join(r"Microsoft\Windows\Start Menu\Programs\Mostalgia.lnk"))
}

/// Sets this process's AppUserModelID; call before any window opens.
pub fn set_process_id() {
    // SAFETY: plain shell call with a valid string.
    if let Err(e) = unsafe { SetCurrentProcessExplicitAppUserModelID(&HSTRING::from(AUMID)) } {
        eprintln!("[mostalgia] app id: {e}");
    }
}

/// Creates or refreshes the Start Menu shortcut (to this exe, with our
/// AppUserModelID), on its own thread so COM there is ours alone.
pub fn ensure_shortcut() {
    std::thread::spawn(|| {
        if let Err(e) = write_shortcut() {
            eprintln!("[mostalgia] Start Menu shortcut: {e:#}");
        }
    });
}

fn write_shortcut() -> Result<()> {
    let path = shortcut_path().context("no %APPDATA%")?;
    let exe = std::env::current_exe()?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    // SAFETY: COM calls on this thread between CoInitializeEx and
    // CoUninitialize; every interface is released before the latter.
    unsafe {
        CoInitializeEx(None, COINIT_MULTITHREADED).ok()?;
        let result = (|| -> Result<()> {
            let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)?;
            link.SetPath(&HSTRING::from(exe.as_os_str()))?;
            if let Some(dir) = exe.parent() {
                link.SetWorkingDirectory(&HSTRING::from(dir.as_os_str()))?;
            }
            link.SetIconLocation(&HSTRING::from(exe.as_os_str()), 0)?;
            link.SetDescription(&HSTRING::from("Mostalgia"))?;
            let store: IPropertyStore = link.cast()?;
            store.SetValue(&PKEY_AppUserModel_ID, &PROPVARIANT::from(AUMID))?;
            store.Commit()?;
            let file: IPersistFile = link.cast()?;
            file.Save(&HSTRING::from(path.as_os_str()), true)?;
            Ok(())
        })();
        CoUninitialize();
        result
    }
}

/// The app id to send toasts with: ours once the shortcut exists, else
/// none (notify-rust's default, which Windows always shows).
pub fn toast_app_id() -> Option<&'static str> {
    shortcut_path().is_some_and(|p| p.exists()).then_some(AUMID)
}
