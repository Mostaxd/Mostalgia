//! Startup check for an NVIDIA card that Whisper can't use. A kernel update
//! that arrives without its NVIDIA module (or a driver install that still
//! needs a reboot) leaves Vulkan with only the integrated GPU, and
//! transcription crawls without saying why.
//!
//! On Windows (Vulkan builds) the same banner covers what goes wrong
//! there: no Vulkan runtime in the system, an NVIDIA driver too old for
//! the Vulkan Whisper needs, or a driver whose Vulkan part doesn't list
//! the card. Windows has no command to fix these; the text says what to do.

use std::path::Path;
use std::process::Command;

/// An NVIDIA card is in the machine but the GPU backend doesn't see it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GpuProblem {
    pub cause: Cause,
    /// What fixes it, when there's a command for it.
    pub fix: Option<Fix>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Cause {
    /// The running kernel has no NVIDIA module (usually: a kernel update
    /// came without it).
    ModuleMissing { kernel: String },
    /// The module is installed for this kernel but not loaded yet.
    RestartNeeded,
    /// The open-source nouveau driver has the card.
    Nouveau,
    /// The NVIDIA driver runs, but Vulkan doesn't list the card (its
    /// Vulkan driver is missing).
    VulkanMissing,
    /// Windows: no Vulkan runtime (`vulkan-1.dll`) in the system: the
    /// graphics driver was installed without it.
    VulkanRuntimeMissing,
    /// Windows: the NVIDIA driver ("456.71") is older than
    /// [`MIN_NVIDIA_DRIVER`].
    DriverTooOld { version: String },
}

/// The oldest NVIDIA driver (Windows numbering) whose Vulkan has what
/// Whisper's Vulkan backend uses.
pub const MIN_NVIDIA_DRIVER: (u32, u32) = (470, 0);

/// A command run as root (with pkexec, or shown with sudo).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fix {
    pub args: Vec<String>,
}

impl Fix {
    fn new(args: &[&str]) -> Self {
        Self { args: args.iter().map(|a| a.to_string()).collect() }
    }

    /// The command to paste into a terminal.
    pub fn shell(&self) -> String {
        format!("sudo {}", self.args.join(" "))
    }

    /// Whether the app can run it itself (pkexec asks for the password).
    pub fn can_run() -> bool {
        Path::new("/usr/bin/pkexec").exists()
    }

    /// Runs the command through pkexec and waits for it. Blocks.
    pub fn run(&self) -> anyhow::Result<()> {
        let out = Command::new("/usr/bin/pkexec")
            .args(&self.args)
            // The AppImage's library path must not leak into apt.
            .env_remove("LD_LIBRARY_PATH")
            .env("DEBIAN_FRONTEND", "noninteractive")
            .output()?;
        if out.status.success() {
            return Ok(());
        }
        let err = String::from_utf8_lossy(&out.stderr);
        let tail: Vec<&str> = err.lines().filter(|l| !l.trim().is_empty()).collect();
        let tail = tail[tail.len().saturating_sub(3)..].join("\n");
        match out.status.code() {
            // pkexec: the password dialog was dismissed or refused.
            Some(126 | 127) => anyhow::bail!("not authorized"),
            _ => anyhow::bail!("{}", if tail.is_empty() { format!("exit status {}", out.status) } else { tail }),
        }
    }
}

/// What the system looks like, gathered once so the decision is testable.
#[derive(Debug, Default)]
struct System {
    /// Kernel drivers bound to the NVIDIA display devices ("" for none).
    nvidia_cards: Vec<String>,
    kernel: String,
    /// The running kernel has an `nvidia.ko`.
    module_installed: bool,
    debian_like: bool,
    /// Installed dpkg packages.
    packages: Vec<String>,
}

/// Checks for the problem. Probes the GPU backend, so run it off the UI
/// thread. `MOSTALGIA_GPU_CHECK=force` treats the backend as if it saw no
/// discrete GPU, to see the banner on a working machine.
pub fn check() -> Option<GpuProblem> {
    #[cfg(windows)]
    {
        if !cfg!(feature = "vulkan") {
            return None;
        }
        let system = windows_system();
        if system.nvidia.is_empty() {
            return None;
        }
        let forced = std::env::var("MOSTALGIA_GPU_CHECK").is_ok_and(|v| v == "force");
        if !forced && crate::transcribe::discrete_gpu().is_some() {
            return None;
        }
        return diagnose_windows(&system);
    }
    #[allow(unreachable_code)]
    if !cfg!(all(target_os = "linux", any(feature = "vulkan", feature = "cuda"))) {
        return None;
    }
    let system = read_system();
    if system.nvidia_cards.is_empty() {
        return None;
    }
    let forced = std::env::var("MOSTALGIA_GPU_CHECK").is_ok_and(|v| v == "force");
    if !forced && crate::transcribe::discrete_gpu().is_some() {
        return None;
    }
    diagnose(&system)
}

fn diagnose(s: &System) -> Option<GpuProblem> {
    let driver = s.nvidia_cards.iter().find(|d| !d.is_empty()).map(String::as_str);
    let (cause, fix) = match driver {
        Some("nvidia") => (Cause::VulkanMissing, None),
        Some("nouveau") => (Cause::Nouveau, s.debian_like.then(|| Fix::new(&["ubuntu-drivers", "install"]))),
        // vfio-pci and the like: on purpose, not ours to touch.
        Some(_) => return None,
        None if s.module_installed => (Cause::RestartNeeded, None),
        None => (Cause::ModuleMissing { kernel: s.kernel.clone() }, s.debian_like.then(|| module_fix(s)).flatten()),
    };
    Some(GpuProblem { cause, fix })
}

/// Ubuntu ships the NVIDIA module prebuilt per kernel
/// (`linux-modules-nvidia-595-open-7.0.0-42-generic`), pulled in by a meta
/// package (`…-595-open-generic-hwe-26.04`) that can lag the kernel. Install
/// both, so the next kernel update brings its module too. With DKMS, rebuild
/// instead.
fn module_fix(s: &System) -> Option<Fix> {
    if s.packages.iter().any(|p| p.starts_with("nvidia-dkms-")) {
        return Some(Fix::new(&["dkms", "autoinstall", "-k", &s.kernel]));
    }
    let mut install = Vec::new();
    for p in &s.packages {
        let Some(rest) = p.strip_prefix("linux-modules-nvidia-") else { continue };
        // "595-open-7.0.0-38-generic" or "595-open-generic-hwe-26.04"
        let Some((branch, flavour)) = split_branch(rest) else { continue };
        if flavour.starts_with(|c: char| c.is_ascii_digit()) {
            let exact = format!("linux-modules-nvidia-{branch}-{}", s.kernel);
            if !install.contains(&exact) {
                install.push(exact);
            }
        } else if !install.contains(p) {
            install.insert(0, p.clone());
        }
    }
    if install.is_empty() {
        return Some(Fix::new(&["ubuntu-drivers", "install"]));
    }
    let mut args = vec!["apt-get", "install", "-y"];
    args.extend(install.iter().map(String::as_str));
    Some(Fix::new(&args))
}

/// "595-open-generic-hwe-26.04" → ("595-open", "generic-hwe-26.04").
fn split_branch(rest: &str) -> Option<(&str, &str)> {
    let digits = rest.find(|c: char| !c.is_ascii_digit())?;
    let after = rest[digits..].strip_prefix('-')?;
    match after.strip_prefix("open-") {
        Some(flavour) => Some((&rest[..digits + 5], flavour)),
        None => Some((&rest[..digits], after)),
    }
}

fn read_system() -> System {
    let mut s = System::default();
    for dev in std::fs::read_dir("/sys/bus/pci/devices").into_iter().flatten().flatten() {
        let read = |f: &str| std::fs::read_to_string(dev.path().join(f)).unwrap_or_default();
        // 0x03xxxx: display controllers.
        if read("vendor").trim() == "0x10de" && read("class").trim().starts_with("0x03") {
            let driver = std::fs::read_link(dev.path().join("driver"))
                .ok()
                .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
                .unwrap_or_default();
            s.nvidia_cards.push(driver);
        }
    }
    if s.nvidia_cards.is_empty() {
        return s;
    }
    s.kernel = std::fs::read_to_string("/proc/sys/kernel/osrelease").unwrap_or_default().trim().to_string();
    let deps = std::fs::read_to_string(format!("/lib/modules/{}/modules.dep", s.kernel)).unwrap_or_default();
    s.module_installed = deps.lines().any(|l| {
        let module = l.split(':').next().unwrap_or_default();
        module.rsplit('/').next().is_some_and(|f| f.starts_with("nvidia.ko"))
    });
    let os = std::fs::read_to_string("/etc/os-release").unwrap_or_default();
    s.debian_like = os.lines().any(|l| {
        (l.starts_with("ID=") || l.starts_with("ID_LIKE=")) && (l.contains("debian") || l.contains("ubuntu"))
    });
    if s.debian_like {
        s.packages = installed_packages(&std::fs::read_to_string("/var/lib/dpkg/status").unwrap_or_default());
    }
    s
}

/// An NVIDIA card as Windows reports it.
#[derive(Debug, Clone)]
struct NvidiaCard {
    #[allow(dead_code)]
    name: String,
    /// Driver version in NVIDIA's numbering, (560, 94).
    driver: Option<(u32, u32)>,
}

#[derive(Debug, Default)]
struct WinSystem {
    nvidia: Vec<NvidiaCard>,
    /// `vulkan-1.dll` is in System32 (installed by the graphics driver).
    vulkan_runtime: bool,
}

#[cfg_attr(not(windows), allow(dead_code))]
fn diagnose_windows(s: &WinSystem) -> Option<GpuProblem> {
    let card = s.nvidia.first()?;
    let cause = if !s.vulkan_runtime {
        Cause::VulkanRuntimeMissing
    } else if let Some(v) = card.driver.filter(|v| *v < MIN_NVIDIA_DRIVER) {
        Cause::DriverTooOld { version: format!("{}.{:02}", v.0, v.1) }
    } else {
        Cause::VulkanMissing
    };
    Some(GpuProblem { cause, fix: None })
}

/// NVIDIA's driver version from the Windows one DXGI reports
/// (32.0.15.6094 → 560.94: the last five digits).
#[cfg_attr(not(windows), allow(dead_code))]
fn nvidia_version(umd: u64) -> (u32, u32) {
    let (c, d) = (((umd >> 16) & 0xffff) as u32, (umd & 0xffff) as u32);
    let n = (c % 10) * 10000 + d;
    (n / 100, n % 100)
}

#[cfg(windows)]
fn windows_system() -> WinSystem {
    use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, IDXGIDevice, IDXGIFactory1};
    use windows::core::Interface;

    let system32 = std::env::var_os("SystemRoot").map(std::path::PathBuf::from).unwrap_or_else(|| "C:\\Windows".into()).join("System32");
    let mut s = WinSystem { vulkan_runtime: system32.join("vulkan-1.dll").exists(), ..Default::default() };
    // SAFETY: plain DXGI queries; the interfaces are released on drop.
    unsafe {
        let Ok(factory) = CreateDXGIFactory1::<IDXGIFactory1>() else { return s };
        let mut i = 0;
        while let Ok(adapter) = factory.EnumAdapters1(i) {
            i += 1;
            let Ok(desc) = adapter.GetDesc1() else { continue };
            if desc.VendorId != 0x10de {
                continue;
            }
            let name = String::from_utf16_lossy(&desc.Description).trim_end_matches('\0').to_string();
            let driver = adapter.CheckInterfaceSupport(&IDXGIDevice::IID).ok().map(|v| nvidia_version(v as u64));
            s.nvidia.push(NvidiaCard { name, driver });
        }
    }
    s
}

/// Package names from dpkg's status file that are installed (or unpacked).
fn installed_packages(status: &str) -> Vec<String> {
    status
        .split("\n\n")
        .filter(|stanza| stanza.lines().any(|l| l.starts_with("Status: install ok")))
        .filter_map(|stanza| stanza.lines().find_map(|l| l.strip_prefix("Package: ")).map(str::to_string))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn system(driver: &str) -> System {
        System {
            nvidia_cards: vec![driver.into()],
            kernel: "7.0.0-42-generic".into(),
            module_installed: false,
            debian_like: true,
            packages: vec![
                "linux-modules-nvidia-595-open-7.0.0-38-generic".into(),
                "linux-modules-nvidia-595-open-generic-hwe-26.04".into(),
                "nvidia-driver-595-open".into(),
            ],
        }
    }

    #[test]
    fn kernel_update_without_its_module() {
        let p = diagnose(&system("")).unwrap();
        assert_eq!(p.cause, Cause::ModuleMissing { kernel: "7.0.0-42-generic".into() });
        assert_eq!(
            p.fix.unwrap().shell(),
            "sudo apt-get install -y linux-modules-nvidia-595-open-generic-hwe-26.04 linux-modules-nvidia-595-open-7.0.0-42-generic"
        );
    }

    #[test]
    fn closed_driver_and_dkms() {
        let mut s = system("");
        s.packages = vec!["linux-modules-nvidia-580-7.0.0-38-generic".into()];
        assert_eq!(diagnose(&s).unwrap().fix.unwrap().args, ["apt-get", "install", "-y", "linux-modules-nvidia-580-7.0.0-42-generic"]);
        s.packages.push("nvidia-dkms-580".into());
        assert_eq!(diagnose(&s).unwrap().fix.unwrap().shell(), "sudo dkms autoinstall -k 7.0.0-42-generic");
    }

    #[test]
    fn other_states() {
        let mut s = system("");
        s.module_installed = true;
        assert_eq!(diagnose(&s).unwrap(), GpuProblem { cause: Cause::RestartNeeded, fix: None });
        assert_eq!(diagnose(&system("nvidia")).unwrap().cause, Cause::VulkanMissing);
        assert_eq!(diagnose(&system("nouveau")).unwrap().fix.unwrap().shell(), "sudo ubuntu-drivers install");
        assert_eq!(diagnose(&system("vfio-pci")), None);
        let mut s = system("");
        s.debian_like = false;
        assert_eq!(diagnose(&s).unwrap().fix, None);
    }

    #[test]
    fn windows_driver_states() {
        let card = |driver| NvidiaCard { name: "NVIDIA GeForce RTX 3060".into(), driver };
        let s = |driver, vulkan_runtime| WinSystem { nvidia: vec![card(driver)], vulkan_runtime };
        assert_eq!(diagnose_windows(&s(Some((560, 94)), false)).unwrap().cause, Cause::VulkanRuntimeMissing);
        assert_eq!(diagnose_windows(&s(Some((456, 71)), true)).unwrap().cause, Cause::DriverTooOld { version: "456.71".into() });
        assert_eq!(diagnose_windows(&s(Some((560, 94)), true)).unwrap().cause, Cause::VulkanMissing);
        assert_eq!(diagnose_windows(&s(None, true)).unwrap().cause, Cause::VulkanMissing);
        assert_eq!(diagnose_windows(&WinSystem::default()), None);
        assert!(diagnose_windows(&s(None, false)).unwrap().fix.is_none());
    }

    #[test]
    fn nvidia_numbering() {
        // 32.0.15.6094
        let umd = (32u64 << 48) | (15 << 16) | 6094;
        assert_eq!(nvidia_version(umd), (560, 94));
        // 27.21.14.5671 → 456.71
        assert_eq!(nvidia_version((27u64 << 48) | (21 << 32) | (14 << 16) | 5671), (456, 71));
    }

    #[test]
    fn reads_dpkg_status() {
        let status = "Package: a\nStatus: install ok installed\n\nPackage: b\nStatus: deinstall ok config-files\n\nPackage: c\nStatus: install ok unpacked\n";
        assert_eq!(installed_packages(status), ["a", "c"]);
    }
}
