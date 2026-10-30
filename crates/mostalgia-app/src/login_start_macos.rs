//! Starting at login on macOS: a LaunchAgent
//! (`~/Library/LaunchAgents/io.github.mostaxd.Mostalgia.plist`) that
//! launches the app minimized when you log in, so calls are noticed without
//! opening it first. macOS loads it at the next login; nothing runs now.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

const LABEL: &str = "io.github.mostaxd.Mostalgia";

fn agent_file() -> Option<PathBuf> {
    Some(dirs_home()?.join("Library/LaunchAgents").join(format!("{LABEL}.plist")))
}

fn dirs_home() -> Option<PathBuf> {
    std::env::var_os("HOME").filter(|h| !h.is_empty()).map(PathBuf::from)
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// The LaunchAgent that starts `exe` minimized at login.
pub fn plist_for(exe: &Path) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>Label</key>
	<string>{LABEL}</string>
	<key>ProgramArguments</key>
	<array>
		<string>{}</string>
		<string>--minimized</string>
	</array>
	<key>RunAtLoad</key>
	<true/>
	<key>ProcessType</key>
	<string>Interactive</string>
</dict>
</plist>
"#,
        xml_escape(&exe.display().to_string())
    )
}

/// Whether the app starts at login (an agent for this very program).
pub fn enabled() -> bool {
    let (Some(file), Ok(exe)) = (agent_file(), std::env::current_exe()) else { return false };
    std::fs::read_to_string(file).is_ok_and(|text| text == plist_for(&exe))
}

pub fn set(on: bool) -> Result<()> {
    let file = agent_file().context("no home folder")?;
    if on {
        let exe = std::env::current_exe().context("finding the app")?;
        if let Some(dir) = file.parent() {
            std::fs::create_dir_all(dir).context("creating ~/Library/LaunchAgents")?;
        }
        std::fs::write(&file, plist_for(&exe)).context("writing the login item")?;
    } else {
        match std::fs::remove_file(&file) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e).context("removing the login item"),
            _ => {}
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_agent_starts_the_app_minimized() {
        let plist = super::plist_for(std::path::Path::new("/Applications/Mostalgia & Co.app/Contents/MacOS/mostalgia-app"));
        assert!(plist.contains("<string>/Applications/Mostalgia &amp; Co.app/Contents/MacOS/mostalgia-app</string>"));
        assert!(plist.contains("<string>--minimized</string>"));
        assert!(plist.contains("<key>RunAtLoad</key>\n\t<true/>"));
    }
}
