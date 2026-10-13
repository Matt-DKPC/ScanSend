//! Which of this computer's addresses a phone can actually reach, and
//! whether a firewall is likely to block it.

use scansend_proto::PairUri;
use std::net::IpAddr;
use std::process::{Command, Stdio};

/// Lower is better; `None` = never offer this address to a phone.
fn rank(iface: &str, ip: &IpAddr) -> Option<u8> {
    let IpAddr::V4(v4) = ip else { return None };
    if v4.is_loopback() || v4.is_link_local() || v4.is_unspecified() {
        return None;
    }
    let n = iface.to_lowercase();
    // Container / VM bridges: unreachable from a phone.
    const VIRTUAL: &[&str] = &[
        "docker", "br-", "veth", "virbr", "vmnet", "vboxnet", "podman", "cni", "flannel", "lxc", "lxd",
        "incus", "vethernet", "wsl", "hyper-v", "virtualbox", "vmware",
    ];
    if VIRTUAL.iter().any(|p| n.starts_with(p) || n.contains(p)) {
        return None;
    }
    let o = v4.octets();
    let vpn = n.contains("tailscale")
        || n.starts_with("ts")
        || n.starts_with("wg")
        || n.starts_with("tun")
        || n.starts_with("zt")
        || (o[0] == 100 && (64..128).contains(&o[1])); // Tailscale / CGNAT range
    Some(if vpn { 2 } else if v4.is_private() { 0 } else { 1 })
}

/// Addresses to offer phones, best first: home LAN, then others, then VPN.
/// This computer's own name ("Front-Desk-PC"), without the ".local" macOS adds.
pub fn device_name() -> String {
    let h = gethostname::gethostname().to_string_lossy().trim().to_string();
    h.strip_suffix(".local").map(str::to_string).unwrap_or(h)
}

pub fn lan_ips() -> Vec<IpAddr> {
    let primary = local_ip_address::local_ip().ok();
    let mut ranked: Vec<(u8, bool, IpAddr)> = local_ip_address::list_afinet_netifas()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|(name, ip)| rank(&name, &ip).map(|r| (r, Some(ip) != primary, ip)))
        .collect();
    ranked.sort();
    let mut out: Vec<IpAddr> = Vec::new();
    for (_, _, ip) in ranked {
        if !out.contains(&ip) {
            out.push(ip);
        }
    }
    out
}

pub fn pair_uri(ips: &[IpAddr], port: u16, pin: &str, name: &str) -> String {
    PairUri {
        hosts: ips.iter().take(4).map(|ip| ip.to_string()).collect(),
        port,
        pin: pin.into(),
        name: name.into(),
    }
    .to_uri()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Firewall {
    Ufw,
    Firewalld,
    Windows,
}

/// The firewall that is probably running here (detection needs no root).
pub fn detect_firewall() -> Option<Firewall> {
    if cfg!(windows) {
        return Some(Firewall::Windows);
    }
    let active = |unit: &str| {
        Command::new("systemctl")
            .args(["is-active", "--quiet", unit])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    };
    if active("firewalld") {
        Some(Firewall::Firewalld)
    } else if active("ufw") {
        Some(Firewall::Ufw)
    } else {
        None
    }
}

impl Firewall {
    pub fn name(self) -> &'static str {
        match self {
            Firewall::Ufw => "ufw",
            Firewall::Firewalld => "firewalld",
            Firewall::Windows => "Windows Firewall",
        }
    }

    /// Command that opens ScanSend's ports, if there is a one-liner for it.
    pub fn open_command(self, http_port: u16) -> String {
        let udp = scansend_proto::DISCOVERY_PORT;
        match self {
            Firewall::Ufw => format!("sudo ufw allow {http_port}/tcp && sudo ufw allow {udp}/udp"),
            Firewall::Firewalld => format!(
                "sudo firewall-cmd --permanent --add-port={http_port}/tcp --add-port={udp}/udp && sudo firewall-cmd --reload"
            ),
            // Windows often labels home Wi-Fi "Public", so a private-only rule
            // silently does nothing; allow every profile but only from the local
            // subnet. Dismissing Windows' own first-run prompt leaves block rules
            // on the exe, and block beats allow, so remove those first.
            Firewall::Windows => {
                let exe = std::env::current_exe().map(|p| p.display().to_string()).unwrap_or_default();
                let fw = "netsh advfirewall firewall";
                let scope = "profile=any remoteip=localsubnet";
                format!(
                    "{fw} delete rule name=all program=\"{exe}\" & {fw} delete rule name=\"ScanSend TCP\" & {fw} delete rule name=\"ScanSend UDP\" & {fw} add rule name=\"ScanSend TCP\" dir=in action=allow protocol=TCP localport={http_port} {scope} & {fw} add rule name=\"ScanSend UDP\" dir=in action=allow protocol=UDP localport={udp} {scope}"
                )
            }
        }
    }

    /// True if both ports are already open. Only firewalld can be asked
    /// without admin rights; for the others this returns false.
    pub fn ports_open(self, http_port: u16) -> bool {
        let udp = scansend_proto::DISCOVERY_PORT;
        self == Firewall::Firewalld
            && [format!("--query-port={http_port}/tcp"), format!("--query-port={udp}/udp")].iter().all(|arg| {
                Command::new("firewall-cmd")
                    .arg(arg)
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status()
                    .map(|s| s.success())
                    .unwrap_or(false)
            })
    }

    /// Open ScanSend's ports, asking for admin rights through the system's own
    /// prompt (polkit on Linux, UAC on Windows). Blocks until the user answers.
    pub fn open_ports(self, http_port: u16) -> Result<(), String> {
        let mut cmd = match self {
            Firewall::Ufw | Firewall::Firewalld => {
                let mut c = Command::new("pkexec");
                c.args(["sh", "-c", &self.open_command(http_port).replace("sudo ", "")]);
                c
            }
            Firewall::Windows => {
                let args = format!("/c {}", self.open_command(http_port)).replace('\'', "''");
                let script = format!(
                    "$p = Start-Process cmd -Verb RunAs -Wait -PassThru -WindowStyle Hidden -ErrorAction Stop -ArgumentList '{args}'; exit $p.ExitCode"
                );
                let mut c = Command::new("powershell");
                c.args(["-NoProfile", "-NonInteractive", "-Command", &script]);
                #[cfg(windows)]
                {
                    use std::os::windows::process::CommandExt;
                    c.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
                }
                c
            }
        };
        let status = cmd
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map_err(|e| format!("Couldn't ask for admin rights: {e}"))?;
        match (self, status.code()) {
            (_, Some(0)) => Ok(()),
            (Firewall::Windows, _) => Err("Not opened: admin permission was declined.".into()),
            // pkexec: 126 = prompt dismissed, 127 = not authorized or no password agent
            (_, Some(126 | 127)) => Err("Not opened: the password prompt was cancelled or isn't available.".into()),
            _ => Err(format!("Not opened: {} reported an error.", self.name())),
        }
    }

    pub fn how_to_run(self) -> &'static str {
        match self {
            Firewall::Windows => "Run it in a Command Prompt opened with \"Run as administrator\", or allow ScanSend when Windows asks.",
            _ => "Run it in a terminal (it asks for your password).",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::rank;

    #[test]
    fn ranks_interfaces() {
        let ip = |s: &str| s.parse().unwrap();
        assert_eq!(rank("enp5s0", &ip("192.168.1.40")), Some(0));
        assert_eq!(rank("wlan0", &ip("192.168.1.9")), Some(0));
        assert_eq!(rank("tailscale0", &ip("100.88.1.2")), Some(2));
        assert_eq!(rank("docker0", &ip("172.17.0.1")), None);
        assert_eq!(rank("br-1a2b3c", &ip("172.18.0.1")), None);
        assert_eq!(rank("vEthernet (WSL)", &ip("172.20.0.1")), None);
        assert_eq!(rank("lo", &ip("127.0.0.1")), None);
    }
}
