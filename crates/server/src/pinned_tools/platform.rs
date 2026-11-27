//! Which prebuilt artifact a machine needs.
//!
//! The product ships on macOS (arm64 first, x86_64 too). Linux is pinned as well so the same
//! code paths run in CI containers and in the development cloud. The selection is a pure function
//! of the OS and architecture names ([`Platform::from_os_arch`]) so every platform's choice is
//! testable on any machine; [`Platform::current`] only feeds it the compile-time values.

/// A machine Camerata can provision tools for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Platform {
    /// macOS on Apple silicon.
    MacArm64,
    /// macOS on Intel.
    MacX64,
    /// Linux (glibc) on x86_64.
    LinuxX64,
    /// Linux (glibc) on aarch64.
    LinuxArm64,
}

impl Platform {
    /// Every platform that has pins, in report order.
    pub const ALL: [Platform; 4] = [
        Platform::MacArm64,
        Platform::MacX64,
        Platform::LinuxX64,
        Platform::LinuxArm64,
    ];

    /// The platform for an OS and architecture as Rust names them (`std::env::consts::OS` and
    /// `std::env::consts::ARCH`). `None` for anything Camerata does not provision for (Windows,
    /// 32-bit, other architectures): every tool then reads as one that cannot be provisioned.
    pub fn from_os_arch(os: &str, arch: &str) -> Option<Platform> {
        match (os, arch) {
            ("macos", "aarch64") => Some(Platform::MacArm64),
            ("macos", "x86_64") => Some(Platform::MacX64),
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
            Platform::MacX64 => "macOS x86_64",
            Platform::LinuxX64 => "Linux x86_64",
            Platform::LinuxArm64 => "Linux arm64",
        }
    }
}

/// The text for a machine with no platform: names what it is so the reason is actionable.
pub fn unsupported_platform_reason() -> String {
    format!(
        "Camerata provisions its tools on macOS and Linux (arm64 and x86_64); this machine is {} {}",
        std::env::consts::OS,
        std::env::consts::ARCH
    )
}
