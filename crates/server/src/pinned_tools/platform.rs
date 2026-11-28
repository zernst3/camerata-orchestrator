//! Which prebuilt artifact a machine needs.
//!
//! The product ships on Apple silicon Macs (macOS arm64), the only Mac target: Intel Macs are not
//! supported (the owner's ruling on #587) and are refused before any model call
//! ([`machine_refusal`]). Linux is pinned as well because the tests and CI run on Linux: the
//! fixtures lay fake tools out exactly as the Linux pins do, and the on-demand first-run harness
//! was run for real on Linux x86_64. The selection is a pure function
//! of the OS and architecture names ([`Platform::from_os_arch`]) so every platform's choice is
//! testable on any machine; [`Platform::current`] only feeds it the compile-time values.

/// A machine Camerata can provision tools for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Platform {
    /// macOS on Apple silicon.
    MacArm64,
    /// Linux (glibc) on x86_64.
    LinuxX64,
    /// Linux (glibc) on aarch64.
    LinuxArm64,
}

impl Platform {
    /// Every platform that has pins, in report order.
    pub const ALL: [Platform; 3] = [Platform::MacArm64, Platform::LinuxX64, Platform::LinuxArm64];

    /// The platform for an OS and architecture as Rust names them (`std::env::consts::OS` and
    /// `std::env::consts::ARCH`). `None` for anything Camerata does not provision for (Windows,
    /// 32-bit, other architectures): every tool then reads as one that cannot be provisioned.
    pub fn from_os_arch(os: &str, arch: &str) -> Option<Platform> {
        match (os, arch) {
            ("macos", "aarch64") => Some(Platform::MacArm64),
            ("linux", "x86_64") => Some(Platform::LinuxX64),
            ("linux", "aarch64") => Some(Platform::LinuxArm64),
            _ => None,
        }
    }

    /// The platform this binary was built for.
    pub fn current() -> Option<Platform> {
        Platform::from_os_arch(std::env::consts::OS, std::env::consts::ARCH)
    }

    /// The name an operator reads in the tool list.
    pub fn label(self) -> &'static str {
        match self {
            Platform::MacArm64 => "macOS arm64",
            Platform::LinuxX64 => "Linux x86_64",
            Platform::LinuxArm64 => "Linux arm64",
        }
    }
}

/// The text for a machine with no platform: names what it is so the reason is actionable.
pub fn unsupported_platform_reason() -> String {
    format!(
        "Camerata provisions its tools on Apple silicon Macs and on Linux (arm64 and x86_64); this \
         machine is {} {}",
        std::env::consts::OS,
        std::env::consts::ARCH
    )
}

/// The one line an Intel Mac is refused with, before any model call: the owner's ruling is that
/// Intel Macs are not supported (macOS arm64 is the only Mac target).
pub const INTEL_MAC_REFUSAL: &str =
    "Intel Macs are not supported: Camerata runs on Apple silicon Macs.";

/// The refusal for a machine Camerata does not run on at all, given the OS and architecture as
/// Rust names them. `None` for every machine it runs on.
pub fn machine_refusal(os: &str, arch: &str) -> Option<&'static str> {
    (os == "macos" && arch == "x86_64").then_some(INTEL_MAC_REFUSAL)
}
