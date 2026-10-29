//! Starting with Windows: an entry under
//! `HKCU\Software\Microsoft\Windows\CurrentVersion\Run` that launches the
//! app minimized at sign-in, so calls are noticed without opening it first.

use anyhow::{Context, Result};
use winreg::RegKey;
use winreg::enums::{HKEY_CURRENT_USER, KEY_READ};

const RUN: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const NAME: &str = "Mostalgia";

/// The command line registered for `exe`.
pub fn command_for(exe: &std::path::Path) -> String {
    format!("\"{}\" --minimized", exe.display())
}

/// Whether the app starts with Windows (an entry for this very program).
pub fn enabled() -> bool {
    let Ok(key) = RegKey::predef(HKEY_CURRENT_USER).open_subkey_with_flags(RUN, KEY_READ) else { return false };
    let Ok(value) = key.get_value::<String, _>(NAME) else { return false };
    std::env::current_exe().is_ok_and(|exe| value == command_for(&exe))
}

pub fn set(on: bool) -> Result<()> {
    let (key, _) = RegKey::predef(HKEY_CURRENT_USER).create_subkey(RUN).context("opening the Run key")?;
    if on {
        let exe = std::env::current_exe().context("finding the app")?;
        key.set_value(NAME, &command_for(&exe)).context("adding the start entry")?;
    } else {
        match key.delete_value(NAME) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e).context("removing the start entry"),
            _ => {}
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_command_quotes_the_path() {
        assert_eq!(
            super::command_for(std::path::Path::new(r"C:\Program Files\Mostalgia\mostalgia-app.exe")),
            r#""C:\Program Files\Mostalgia\mostalgia-app.exe" --minimized"#
        );
    }
}
