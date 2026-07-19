//! Install system `xorriso` via the distro package manager (Linux).

use std::path::Path;
use std::process::Command;

/// How to install the `xorriso` package on this machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackageManager {
    Apt,
    Dnf,
    Pacman,
    Zypper,
    Apk,
}

impl PackageManager {
    pub fn detect() -> Option<Self> {
        // Prefer the manager that matches os-release when several exist.
        let id = os_release_id().unwrap_or_default();
        let id = id.as_str();

        if matches!(id, "debian" | "ubuntu" | "linuxmint" | "pop" | "elementary" | "raspbian")
            && which("apt-get").is_some()
        {
            return Some(Self::Apt);
        }
        if matches!(id, "fedora" | "rhel" | "centos" | "rocky" | "almalinux" | "nobara")
            && which("dnf").or_else(|| which("yum")).is_some()
        {
            return Some(Self::Dnf);
        }
        if matches!(id, "arch" | "manjaro" | "endeavouros" | "garuda" | "cachyos")
            && which("pacman").is_some()
        {
            return Some(Self::Pacman);
        }
        if matches!(id, "opensuse" | "opensuse-tumbleweed" | "opensuse-leap" | "suse")
            && which("zypper").is_some()
        {
            return Some(Self::Zypper);
        }
        if id == "alpine" && which("apk").is_some() {
            return Some(Self::Apk);
        }

        // Fall back to whichever tool is on PATH.
        if which("apt-get").is_some() {
            Some(Self::Apt)
        } else if which("dnf").is_some() {
            Some(Self::Dnf)
        } else if which("pacman").is_some() {
            Some(Self::Pacman)
        } else if which("zypper").is_some() {
            Some(Self::Zypper)
        } else if which("apk").is_some() {
            Some(Self::Apk)
        } else {
            None
        }
    }

    pub fn package_name(self) -> &'static str {
        "xorriso"
    }

    pub fn short_command(self) -> String {
        match self {
            Self::Apt => "sudo apt install xorriso".into(),
            Self::Dnf => "sudo dnf install xorriso".into(),
            Self::Pacman => "sudo pacman -S xorriso".into(),
            Self::Zypper => "sudo zypper install xorriso".into(),
            Self::Apk => "sudo apk add xorriso".into(),
        }
    }

    /// Program + args for an elevated, non-interactive install.
    pub fn elevated_argv(self) -> Option<(String, Vec<String>)> {
        let pkg = self.package_name().to_string();
        let (inner_prog, mut inner_args) = match self {
            Self::Apt => (
                "apt-get".into(),
                vec![
                    "install".into(),
                    "-y".into(),
                    pkg,
                ],
            ),
            Self::Dnf => {
                let prog = which("dnf")
                    .map(|_| "dnf".into())
                    .unwrap_or_else(|| "yum".into());
                (prog, vec!["install".into(), "-y".into(), pkg])
            }
            Self::Pacman => (
                "pacman".into(),
                vec!["-S".into(), "--noconfirm".into(), "--needed".into(), pkg],
            ),
            Self::Zypper => (
                "zypper".into(),
                vec!["--non-interactive".into(), "install".into(), pkg],
            ),
            Self::Apk => ("apk".into(), vec!["add".into(), pkg]),
        };

        // Prefer a graphical polkit prompt when available.
        if which("pkexec").is_some() {
            if matches!(self, Self::Apt) {
                return Some((
                    "pkexec".into(),
                    vec![
                        "env".into(),
                        "DEBIAN_FRONTEND=noninteractive".into(),
                        "apt-get".into(),
                        "install".into(),
                        "-y".into(),
                        self.package_name().into(),
                    ],
                ));
            }
            let mut args = vec![inner_prog];
            args.append(&mut inner_args);
            return Some(("pkexec".into(), args));
        }

        // Fallback: sudo (may prompt in a terminal).
        if which("sudo").is_some() {
            let mut args = vec![inner_prog];
            args.append(&mut inner_args);
            return Some(("sudo".into(), args));
        }

        None
    }
}

/// Attempt to install xorriso using the detected package manager.
///
/// This blocks until the privileged install finishes (pkexec/sudo prompt).
pub fn install_xorriso() -> Result<String, String> {
    #[cfg(not(target_os = "linux"))]
    {
        return Err("Automatic xorriso install is only supported on Linux.".into());
    }

    #[cfg(target_os = "linux")]
    {
        let mgr = PackageManager::detect().ok_or_else(|| {
            "No supported package manager found (apt, dnf, pacman, zypper, or apk). \
             Install xorriso manually, or place a binary at vendor/xorriso next to the app."
                .to_string()
        })?;

        let (prog, args) = mgr.elevated_argv().ok_or_else(|| {
            format!(
                "Need pkexec or sudo to install packages. Run manually:\n  {}",
                mgr.short_command()
            )
        })?;

        let display = format!("{prog} {}", args.join(" "));
        let output = Command::new(&prog)
            .args(&args)
            .output()
            .map_err(|e| {
                format!(
                    "Failed to start installer ({display}): {e}\n\
                     Try manually: {}",
                    mgr.short_command()
                )
            })?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        if output.status.success() {
            // Confirm the binary is now resolvable.
            if super::xorriso::find_xorriso().is_some() {
                Ok(format!("Installed xorriso via {display}"))
            } else {
                Err(format!(
                    "Package install finished but xorriso is still not on PATH.\n{stdout}\n{stderr}"
                ))
            }
        } else {
            let code = output.status.code().unwrap_or(-1);
            Err(format!(
                "Install failed (exit {code}).\nCommand: {display}\n{stderr}\n{stdout}\n\
                 You can also run:\n  {}",
                mgr.short_command()
            ))
        }
    }
}

fn os_release_id() -> Option<String> {
    let data = std::fs::read_to_string("/etc/os-release").ok()?;
    for line in data.lines() {
        if let Some(rest) = line.strip_prefix("ID=") {
            return Some(rest.trim_matches('"').to_ascii_lowercase());
        }
    }
    None
}

fn which(name: &str) -> Option<std::path::PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        let candidate = Path::new(&dir).join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_commands_mention_xorriso() {
        for mgr in [
            PackageManager::Apt,
            PackageManager::Dnf,
            PackageManager::Pacman,
            PackageManager::Zypper,
            PackageManager::Apk,
        ] {
            assert!(mgr.short_command().contains("xorriso"));
        }
    }
}
