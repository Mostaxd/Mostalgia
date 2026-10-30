//! Call detection on macOS: which apps record from a microphone.
//!
//! macOS 14 and newer list Core Audio's process objects and say for each
//! whether it is running input, with its PID and bundle ID, which is what
//! PipeWire tells us on Linux. Before 14, Core Audio only says whether the
//! default input device is in use by anyone; then the running call apps
//! are the guess.

use anyhow::Result;
use objc2_core_audio::{
    AudioObjectID, kAudioDevicePropertyDeviceIsRunningSomewhere, kAudioHardwarePropertyDefaultInputDevice,
    kAudioHardwarePropertyProcessObjectList, kAudioObjectSystemObject, kAudioProcessPropertyBundleID, kAudioProcessPropertyIsRunningInput,
    kAudioProcessPropertyPID,
};

use crate::audio_macos::{get_list, get_property, get_string};
use super::{CALL_APPS, CaptureStream, classify};

const SYSTEM: AudioObjectID = kAudioObjectSystemObject as AudioObjectID;

/// Polls Core Audio; nothing to keep open between polls.
pub(crate) struct Watcher;

impl Watcher {
    pub(crate) fn connect() -> Result<Self> {
        Ok(Self)
    }

    /// The apps recording from a microphone now.
    pub(crate) fn capture_streams(&mut self) -> Result<Vec<CaptureStream>> {
        match get_list::<AudioObjectID>(SYSTEM, kAudioHardwarePropertyProcessObjectList) {
            Ok(processes) => Ok(input_processes(&processes)),
            // Before macOS 14.
            Err(_) => Ok(guess_from_running_apps()),
        }
    }
}

fn input_processes(processes: &[AudioObjectID]) -> Vec<CaptureStream> {
    let me = std::process::id() as i32;
    processes
        .iter()
        .filter(|&&p| get_property::<u32, ()>(p, kAudioProcessPropertyIsRunningInput, None).is_ok_and(|r| r != 0))
        .filter_map(|&p| {
            let pid = get_property::<i32, ()>(p, kAudioProcessPropertyPID, None).ok()?;
            if pid == me {
                return None;
            }
            let bundle = get_string(p, kAudioProcessPropertyBundleID).map(|s| s.to_string()).unwrap_or_default();
            Some(CaptureStream { app_name: process_name(pid), binary: bundle, media_name: String::new(), from_monitor: false })
        })
        .collect()
}

/// The executable's name of a process ("zoom.us", "Google Chrome Helper").
fn process_name(pid: i32) -> String {
    let mut buf = [0u8; 256];
    let n = unsafe { libc::proc_name(pid, buf.as_mut_ptr().cast(), buf.len() as u32) };
    if n <= 0 {
        return String::new();
    }
    String::from_utf8_lossy(&buf[..n as usize]).into_owned()
}

/// Before macOS 14: when the default microphone is in use, the running
/// call apps (or, without one, browsers) are taken to be on a call.
fn guess_from_running_apps() -> Vec<CaptureStream> {
    let Ok(input) = get_property::<AudioObjectID, ()>(SYSTEM, kAudioHardwarePropertyDefaultInputDevice, None) else {
        return Vec::new();
    };
    if input == 0 || !get_property::<u32, ()>(input, kAudioDevicePropertyDeviceIsRunningSomewhere, None).is_ok_and(|r| r != 0) {
        return Vec::new();
    }
    pick_running(running_process_names())
}

/// From running processes' names, the call apps, or the browsers if no call
/// app runs (one stream per app).
pub(crate) fn pick_running(names: Vec<String>) -> Vec<CaptureStream> {
    let stream = |name: String| CaptureStream { app_name: name.clone(), binary: name, media_name: String::new(), from_monitor: false };
    let mut seen = std::collections::HashSet::new();
    let found: Vec<(String, CaptureStream)> = names
        .into_iter()
        .map(stream)
        .filter_map(|s| classify(&s).map(|label| (label, s)))
        .filter(|(label, _)| seen.insert(label.clone()))
        .collect();
    let is_call = |s: &CaptureStream| {
        let name = s.app_name.to_lowercase();
        CALL_APPS.iter().any(|(_, needles)| needles.iter().any(|n| name.contains(n)))
    };
    let calls: Vec<CaptureStream> = found.iter().filter(|(_, s)| is_call(s)).map(|(_, s)| s.clone()).collect();
    if calls.is_empty() { found.into_iter().map(|(_, s)| s).collect() } else { calls }
}

fn running_process_names() -> Vec<String> {
    let n = unsafe { libc::proc_listallpids(std::ptr::null_mut(), 0) };
    if n <= 0 {
        return Vec::new();
    }
    let mut pids = vec![0i32; n as usize + 64];
    let bytes = (pids.len() * std::mem::size_of::<i32>()) as libc::c_int;
    let n = unsafe { libc::proc_listallpids(pids.as_mut_ptr().cast(), bytes) };
    let me = std::process::id() as i32;
    pids.truncate(n.max(0) as usize);
    pids.into_iter().filter(|&p| p > 0 && p != me).map(process_name).filter(|n| !n.is_empty()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn running_call_apps_win_over_browsers() {
        let names = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let labels = |v: Vec<CaptureStream>| v.iter().filter_map(classify).collect::<Vec<_>>();
        assert_eq!(labels(pick_running(names(&["Finder", "zoom.us", "Google Chrome", "Google Chrome Helper", "MSTeams"]))), vec!["Zoom", "Microsoft Teams"]);
        let browsers = labels(pick_running(names(&["Finder", "Google Chrome", "Google Chrome Helper"])));
        assert_eq!(browsers.len(), 1);
        assert!(browsers[0].ends_with("(Chrome)"));
        assert!(pick_running(names(&["Finder", "Dock"])).is_empty());
    }

    /// Only runs Core Audio queries; on CI there's no call, so nothing
    /// (or our own test process, which is left out) records.
    #[test]
    fn polling_core_audio_works() {
        let mut w = Watcher::connect().unwrap();
        let streams = w.capture_streams().unwrap();
        assert!(streams.iter().all(|s| !s.app_name.starts_with("mostalgia")));
    }
}
