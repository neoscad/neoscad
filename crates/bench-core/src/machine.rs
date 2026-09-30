//! The machine a community result was measured on, as far as the OS says
//! cheaply: enough to compare like with like, and nothing that identifies
//! a person or a host. No hostname, user name, path, serial number, MAC
//! or IP address is ever read here, so none can reach a result; the
//! `no_identifying_fields` test checks the finished payload too.
//!
//! Every probe is best-effort: a field the OS does not answer is `null`
//! rather than an error, because a missing CPU name should not cost the
//! user a benchmark run.

use std::process::Command;

use serde::{Deserialize, Serialize};

/// Hardware and OS facts of one run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Machine {
    /// `macos`, `linux`, `windows` (Rust's `std::env::consts::OS`).
    pub os: String,
    /// The OS's own name and version, e.g. "macOS 15.6 (24G84)",
    /// "Ubuntu 22.04.5 LTS (kernel 6.8.0-1021-azure)",
    /// "Microsoft Windows [Version 10.0.26100.4652]".
    pub os_version: Option<String>,
    /// `aarch64`, `x86_64` (`std::env::consts::ARCH`): the binary's
    /// architecture, which is the machine's unless it runs translated.
    pub arch: String,
    /// The CPU's marketing name, e.g. "Apple M3 Pro".
    pub cpu: Option<String>,
    /// A Mac's model identifier (e.g. "Mac15,6"); null elsewhere.
    pub hardware_model: Option<String>,
    /// Logical CPUs the process may use.
    pub cores_logical: u32,
    pub cores_physical: Option<u32>,
    /// Apple silicon's performance and efficiency cores; null elsewhere.
    pub cores_performance: Option<u32>,
    pub cores_efficiency: Option<u32>,
    pub memory_bytes: Option<u64>,
    /// On battery power when the run started (a laptop throttles); null
    /// where it is not cheap to ask (Windows).
    pub on_battery: Option<bool>,
    /// 1, 5 and 15 minute load averages before and after the run; null
    /// on Windows, which has none.
    pub load_before: Option<[f64; 3]>,
    pub load_after: Option<[f64; 3]>,
    /// Whether an x86_64 binary is running under Rosetta 2.
    pub translated: Option<bool>,
}

/// Probe the machine now (`load_after` is left null; see [`load_average`]).
pub fn probe() -> Machine {
    let cores_logical = std::thread::available_parallelism().map_or(1, |n| n.get() as u32);
    let mut m = Machine {
        os: std::env::consts::OS.to_string(),
        os_version: None,
        arch: std::env::consts::ARCH.to_string(),
        cpu: None,
        hardware_model: None,
        cores_logical,
        cores_physical: None,
        cores_performance: None,
        cores_efficiency: None,
        memory_bytes: None,
        on_battery: None,
        load_before: load_average(),
        load_after: None,
        translated: None,
    };
    if cfg!(target_os = "macos") {
        macos(&mut m);
    } else if cfg!(target_os = "linux") {
        linux(&mut m);
    } else if cfg!(windows) {
        windows(&mut m);
    }
    m
}

/// The 1/5/15 minute load averages, or `None` (Windows).
pub fn load_average() -> Option<[f64; 3]> {
    let text = if cfg!(target_os = "macos") {
        // "{ 1.93 2.04 2.13 }"
        sysctl("vm.loadavg")?
    } else if cfg!(target_os = "linux") {
        std::fs::read_to_string("/proc/loadavg").ok()?
    } else {
        return None;
    };
    parse_load(&text)
}

fn parse_load(text: &str) -> Option<[f64; 3]> {
    let v: Vec<f64> = text
        .split_whitespace()
        .filter_map(|w| w.parse().ok())
        .take(3)
        .collect();
    (v.len() == 3).then(|| [v[0], v[1], v[2]])
}

/// Stdout of a command, trimmed; `None` when it fails or says nothing.
fn stdout_of(cmd: &str, args: &[&str]) -> Option<String> {
    let o = Command::new(cmd).args(args).output().ok()?;
    let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
    (o.status.success() && !s.is_empty()).then_some(s)
}

fn sysctl(key: &str) -> Option<String> {
    stdout_of("sysctl", &["-n", key])
}

fn sysctl_u(key: &str) -> Option<u32> {
    sysctl(key)?.parse().ok()
}

fn macos(m: &mut Machine) {
    m.cpu = sysctl("machdep.cpu.brand_string");
    m.hardware_model = sysctl("hw.model");
    m.cores_physical = sysctl_u("hw.physicalcpu");
    // Only Apple silicon has two performance levels.
    if sysctl_u("hw.nperflevels") == Some(2) {
        m.cores_performance = sysctl_u("hw.perflevel0.physicalcpu");
        m.cores_efficiency = sysctl_u("hw.perflevel1.physicalcpu");
    }
    m.memory_bytes = sysctl("hw.memsize").and_then(|s| s.parse().ok());
    m.os_version = match (
        stdout_of("sw_vers", &["-productVersion"]),
        stdout_of("sw_vers", &["-buildVersion"]),
    ) {
        (Some(v), Some(b)) => Some(format!("macOS {v} ({b})")),
        (Some(v), None) => Some(format!("macOS {v}")),
        _ => None,
    };
    m.on_battery = stdout_of("pmset", &["-g", "batt"]).and_then(|s| {
        if s.contains("'Battery Power'") {
            Some(true)
        } else if s.contains("'AC Power'") {
            Some(false)
        } else {
            None
        }
    });
    m.translated = sysctl("sysctl.proc_translated").map(|s| s == "1");
}

fn linux(m: &mut Machine) {
    let cpuinfo = std::fs::read_to_string("/proc/cpuinfo").unwrap_or_default();
    m.cpu = cpuinfo_field(&cpuinfo, "model name").or_else(|| {
        // aarch64 kernels print no model name; lscpu decodes the part.
        stdout_of("lscpu", &[]).and_then(|s| cpuinfo_field(&s, "Model name"))
    });
    m.cores_physical = linux_physical_cores(&cpuinfo);
    m.memory_bytes = std::fs::read_to_string("/proc/meminfo")
        .ok()
        .and_then(|s| cpuinfo_field(&s, "MemTotal"))
        .and_then(|v| v.trim_end_matches(" kB").trim().parse::<u64>().ok())
        .map(|kb| kb * 1024);
    let pretty = std::fs::read_to_string("/etc/os-release")
        .ok()
        .and_then(|s| {
            s.lines()
                .find_map(|l| l.strip_prefix("PRETTY_NAME="))
                .map(|v| v.trim_matches('"').to_string())
        });
    let kernel = std::fs::read_to_string("/proc/sys/kernel/osrelease")
        .ok()
        .map(|s| s.trim().to_string());
    m.os_version = match (pretty, kernel) {
        (Some(p), Some(k)) => Some(format!("{p} (kernel {k})")),
        (Some(p), None) => Some(p),
        (None, Some(k)) => Some(format!("Linux {k}")),
        (None, None) => None,
    };
    m.on_battery = linux_on_battery();
}

/// `key : value` of the first matching line.
fn cpuinfo_field(text: &str, key: &str) -> Option<String> {
    text.lines().find_map(|l| {
        let (k, v) = l.split_once(':')?;
        (k.trim() == key)
            .then(|| v.trim().to_string())
            .filter(|v| !v.is_empty())
    })
}

/// Distinct (physical id, core id) pairs; `None` when the kernel lists
/// neither (most aarch64 kernels).
fn linux_physical_cores(cpuinfo: &str) -> Option<u32> {
    let mut pairs = std::collections::BTreeSet::new();
    let mut phys = String::new();
    for l in cpuinfo.lines() {
        if let Some((k, v)) = l.split_once(':') {
            match k.trim() {
                "physical id" => phys = v.trim().to_string(),
                "core id" => {
                    pairs.insert((phys.clone(), v.trim().to_string()));
                }
                _ => {}
            }
        }
    }
    (!pairs.is_empty()).then_some(pairs.len() as u32)
}

/// Battery if the kernel lists a battery and no mains supply is online.
fn linux_on_battery() -> Option<bool> {
    let dir = std::fs::read_dir("/sys/class/power_supply").ok()?;
    let (mut battery, mut mains_online) = (false, false);
    for e in dir.flatten() {
        let p = e.path();
        let kind = std::fs::read_to_string(p.join("type")).unwrap_or_default();
        match kind.trim() {
            "Battery" => battery = true,
            "Mains" | "USB"
                if std::fs::read_to_string(p.join("online")).is_ok_and(|s| s.trim() == "1") =>
            {
                mains_online = true;
            }
            _ => {}
        }
    }
    Some(battery && !mains_online)
}

fn windows(m: &mut Machine) {
    m.os_version = stdout_of("cmd", &["/C", "ver"]);
    // One PowerShell start for the CPU, core and memory facts (wmic is
    // gone from current Windows); about half a second, once per run.
    let script = "$c = Get-CimInstance Win32_Processor | Select-Object -First 1; \
                  $s = Get-CimInstance Win32_ComputerSystem; \
                  \"$($c.Name)|$($s.NumberOfProcessors)|$((Get-CimInstance Win32_Processor | Measure-Object -Property NumberOfCores -Sum).Sum)|$($s.TotalPhysicalMemory)\"";
    if let Some(out) = stdout_of("powershell", &["-NoProfile", "-Command", script]) {
        let f: Vec<&str> = out.split('|').collect();
        if f.len() == 4 {
            m.cpu = Some(f[0].trim().to_string()).filter(|s| !s.is_empty());
            m.cores_physical = f[2].trim().parse().ok();
            m.memory_bytes = f[3].trim().parse().ok();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_load_averages() {
        assert_eq!(parse_load("{ 1.93 2.04 2.13 }"), Some([1.93, 2.04, 2.13]));
        assert_eq!(
            parse_load("0.52 0.58 0.59 1/977 12345\n"),
            Some([0.52, 0.58, 0.59])
        );
        assert_eq!(parse_load("nothing"), None);
    }

    #[test]
    fn counts_linux_physical_cores() {
        let info = "processor : 0\nphysical id : 0\ncore id : 0\n\nprocessor : 1\nphysical id : 0\ncore id : 0\n\nprocessor : 2\nphysical id : 0\ncore id : 1\n";
        assert_eq!(linux_physical_cores(info), Some(2));
        assert_eq!(linux_physical_cores("processor : 0\n"), None);
        assert_eq!(
            cpuinfo_field("model name\t: AMD EPYC 7763\n", "model name").as_deref(),
            Some("AMD EPYC 7763")
        );
    }

    #[test]
    fn probe_answers_the_basics() {
        let m = probe();
        assert!(m.cores_logical >= 1);
        assert_eq!(m.os, std::env::consts::OS);
    }
}
