//! A quick look at the machine, used to offer command models that fit.
//!
//! The GPU comes from the renderer's wgpu adapter when one is available; the
//! rest is read from the OS. Nothing here is sent anywhere. The numbers only
//! choose which models to offer and which one to recommend. Fit is based on
//! system RAM: Linux builds run llama.cpp on the CPU, and Apple Silicon shares
//! memory between the CPU and GPU.

#[derive(Clone, Debug)]
pub struct Hardware {
    pub ram_bytes: u64,
    pub threads: usize,
    pub cpu: Option<String>,
    pub gpu: Option<String>,
}

impl Hardware {
    pub fn detect(gpu: Option<String>) -> Self {
        let gpu = gpu.filter(|name| !is_software_adapter(name));
        Self {
            ram_bytes: total_ram().unwrap_or(8 * GIB),
            threads: std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(1),
            cpu: cpu_name(),
            gpu: gpu.or_else(system_gpu),
        }
    }

    pub fn ram_gb(&self) -> f64 {
        self.ram_bytes as f64 / GIB as f64
    }

    /// One line for the model picker, e.g. "32 GB RAM · Apple M3 Pro".
    pub fn summary(&self) -> String {
        let mut text = format!("{:.0} GB RAM", self.ram_gb());
        if let Some(gpu) = &self.gpu {
            text.push_str(" · ");
            text.push_str(gpu);
        } else if let Some(cpu) = &self.cpu {
            text.push_str(" · ");
            text.push_str(cpu);
        } else if self.threads > 0 {
            text.push_str(&format!(" · {} CPU threads", self.threads));
        }
        text
    }
}

pub const GIB: u64 = 1_073_741_824;

fn is_software_adapter(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    [
        "llvmpipe",
        "softpipe",
        "swiftshader",
        "software",
        "basic render",
    ]
    .iter()
    .any(|marker| name.contains(marker))
}

#[cfg(target_os = "linux")]
fn total_ram() -> Option<u64> {
    let text = std::fs::read_to_string("/proc/meminfo").ok()?;
    let line = text.lines().find(|line| line.starts_with("MemTotal:"))?;
    let kb: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
    Some(kb.saturating_mul(1024))
}

#[cfg(target_os = "macos")]
fn total_ram() -> Option<u64> {
    sysctl("hw.memsize").and_then(|text| text.parse().ok())
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn total_ram() -> Option<u64> {
    None
}

#[cfg(target_os = "linux")]
fn cpu_name() -> Option<String> {
    let text = std::fs::read_to_string("/proc/cpuinfo").ok()?;
    for line in text.lines() {
        if let Some(value) = line.strip_prefix("model name") {
            let value = value.split_once(':').map(|(_, value)| value)?;
            let value = value.trim();
            if !value.is_empty() {
                return Some(value.to_string());
            }
        }
    }
    None
}

#[cfg(target_os = "macos")]
fn cpu_name() -> Option<String> {
    sysctl("machdep.cpu.brand_string").filter(|name| !name.is_empty())
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn cpu_name() -> Option<String> {
    None
}

#[cfg(target_os = "macos")]
fn sysctl(key: &str) -> Option<String> {
    let output = std::process::Command::new("sysctl")
        .args(["-n", key])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8(output.stdout).ok()?;
    Some(text.trim().to_string())
}

/// When the GPU renderer is disabled we can still name a Linux GPU from sysfs.
#[cfg(target_os = "linux")]
fn system_gpu() -> Option<String> {
    let entries = std::fs::read_dir("/sys/class/drm").ok()?;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        // "card0" is a GPU; "card0-DP-1" is one of its connectors.
        if !name.starts_with("card") || name.contains('-') {
            continue;
        }
        let vendor = std::fs::read_to_string(entry.path().join("device/vendor"))
            .ok()
            .map(|v| v.trim().to_string());
        let label = match vendor.as_deref() {
            Some("0x10de") => "NVIDIA GPU",
            Some("0x1002") | Some("0x1022") => "AMD GPU",
            Some("0x8086") => "Intel GPU",
            _ => continue,
        };
        return Some(label.to_string());
    }
    None
}

#[cfg(not(target_os = "linux"))]
fn system_gpu() -> Option<String> {
    None
}

/// macOS 27 has a system model. Even when Apple Intelligence is disabled, the
/// user should not be asked to pick a downloaded Qwen model on these Macs.
#[cfg(target_os = "macos")]
pub fn is_macos_27() -> bool {
    use std::sync::OnceLock;
    static MACOS_27: OnceLock<bool> = OnceLock::new();
    *MACOS_27.get_or_init(|| macos_major_version().is_some_and(|major| major >= 27))
}

#[cfg(target_os = "macos")]
fn macos_major_version() -> Option<u32> {
    let output = std::process::Command::new("sw_vers")
        .args(["-productVersion"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8(output.stdout).ok()?;
    text.trim().split('.').next()?.parse().ok()
}

#[cfg(not(target_os = "macos"))]
pub fn is_macos_27() -> bool {
    false
}
