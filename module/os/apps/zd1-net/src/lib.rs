//! Network interfaces the way `ifconfig` and `route` do it, by ioctl on a socket: a MAC, up and down, one IPv4
//! address, the default route, the name servers. What init and zd1-dhcp need, and nothing else (2.6.24: no netlink
//! worth using from here). And the Wi-Fi supplicant's control socket (`wpa`), for zd1-linkd.

use std::io;
use std::net::Ipv4Addr;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

/// The name servers' file: /etc/resolv.conf points here (/ is read-only).
pub const RESOLV_CONF: &str = "/run/resolv.conf";

const IFNAMSIZ: usize = 16;
#[cfg(target_os = "linux")]
const ARPHRD_ETHER: u16 = 1;

/// `struct ifreq`: the name, then a 16-byte union (an address, the flags, ...).
#[repr(C)]
struct IfReq {
    name: [u8; IFNAMSIZ],
    data: [u8; 16],
}

/// An interface, and the socket its ioctls go through.
pub struct Iface {
    name: String,
    sock: OwnedFd,
}

impl Iface {
    pub fn open(name: &str) -> io::Result<Iface> {
        if name.len() >= IFNAMSIZ {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "interface name too long"));
        }
        let fd = unsafe { libc::socket(libc::AF_INET, libc::SOCK_DGRAM, 0) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Iface { name: name.into(), sock: unsafe { OwnedFd::from_raw_fd(fd) } })
    }

    /// Whether the kernel has it (its driver loaded).
    pub fn exists(name: &str) -> bool {
        std::path::Path::new("/sys/class/net").join(name).exists()
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    fn req(&self) -> IfReq {
        let mut r = IfReq { name: [0; IFNAMSIZ], data: [0; 16] };
        r.name[..self.name.len()].copy_from_slice(self.name.as_bytes());
        r
    }

    fn ioctl(&self, what: libc::c_ulong, r: &mut IfReq) -> io::Result<()> {
        if unsafe { libc::ioctl(self.sock.as_raw_fd(), what as _, r as *mut IfReq) } < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    fn flags(&self) -> io::Result<i16> {
        let mut r = self.req();
        self.ioctl(libc::SIOCGIFFLAGS as _, &mut r)?;
        Ok(i16::from_ne_bytes([r.data[0], r.data[1]]))
    }

    fn set_flags(&self, f: i16) -> io::Result<()> {
        let mut r = self.req();
        r.data[..2].copy_from_slice(&f.to_ne_bytes());
        self.ioctl(libc::SIOCSIFFLAGS as _, &mut r)
    }

    /// Up. For eth0 this is when the driver probes the PHY: it fails while the PHY doesn't answer.
    pub fn up(&self) -> io::Result<()> {
        self.set_flags(self.flags()? | libc::IFF_UP as i16)
    }

    pub fn down(&self) -> io::Result<()> {
        self.set_flags(self.flags()? & !(libc::IFF_UP as i16))
    }

    // The MAC and the routes are Linux ioctls (none on macOS): without them the crate still builds on any host, so
    // the supplicant's parsers (`wpa`) have their tests there.
    #[cfg(target_os = "linux")]
    pub fn set_mac(&self, mac: [u8; 6]) -> io::Result<()> {
        let mut r = self.req();
        r.data[..2].copy_from_slice(&ARPHRD_ETHER.to_ne_bytes());
        r.data[2..8].copy_from_slice(&mac);
        self.ioctl(libc::SIOCSIFHWADDR as _, &mut r)
    }

    /// The address and its netmask (0.0.0.0: none).
    pub fn set_ipv4(&self, addr: Ipv4Addr, mask: Ipv4Addr) -> io::Result<()> {
        let mut r = self.req();
        sockaddr_in(&mut r.data, addr);
        self.ioctl(libc::SIOCSIFADDR as _, &mut r)?;
        if addr.is_unspecified() {
            return Ok(());
        }
        let mut r = self.req();
        sockaddr_in(&mut r.data, mask);
        self.ioctl(libc::SIOCSIFNETMASK as _, &mut r)
    }

    pub fn ipv4(&self) -> Option<Ipv4Addr> {
        let mut r = self.req();
        self.ioctl(libc::SIOCGIFADDR as _, &mut r).ok()?;
        let a = Ipv4Addr::new(r.data[4], r.data[5], r.data[6], r.data[7]);
        (!a.is_unspecified()).then_some(a)
    }

    /// The interface's IPv4 netmask.
    pub fn ipv4_mask(&self) -> Option<Ipv4Addr> {
        let mut r = self.req();
        self.ioctl(libc::SIOCGIFNETMASK as _, &mut r).ok()?;
        Some(Ipv4Addr::new(r.data[4], r.data[5], r.data[6], r.data[7]))
    }

    /// The link has carrier (a cable plugged in, associated on Wi-Fi).
    pub fn carrier(&self) -> bool {
        std::fs::read_to_string(format!("/sys/class/net/{}/carrier", self.name))
            .map(|s| s.trim() == "1")
            .unwrap_or(false)
    }

    /// The default route through `gw` on this interface.
    #[cfg(target_os = "linux")]
    pub fn add_default_route(&self, gw: Ipv4Addr) -> io::Result<()> {
        self.route(libc::SIOCADDRT as _, Some(gw))
    }

    /// Every default route on this interface gone.
    #[cfg(target_os = "linux")]
    pub fn remove_default_routes(&self) {
        while self.route(libc::SIOCDELRT as _, None).is_ok() {}
    }

    #[cfg(target_os = "linux")]
    fn route(&self, what: libc::c_ulong, gw: Option<Ipv4Addr>) -> io::Result<()> {
        let mut rt: libc::rtentry = unsafe { std::mem::zeroed() };
        let dev = std::ffi::CString::new(self.name.as_str()).unwrap();
        set_sockaddr(&mut rt.rt_dst, Ipv4Addr::UNSPECIFIED);
        set_sockaddr(&mut rt.rt_genmask, Ipv4Addr::UNSPECIFIED);
        rt.rt_flags = libc::RTF_UP;
        if let Some(gw) = gw {
            set_sockaddr(&mut rt.rt_gateway, gw);
            rt.rt_flags |= libc::RTF_GATEWAY;
        }
        rt.rt_dev = dev.as_ptr() as *mut _;
        if unsafe { libc::ioctl(self.sock.as_raw_fd(), what as _, &mut rt as *mut libc::rtentry) } < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}

fn sockaddr_in(b: &mut [u8; 16], a: Ipv4Addr) {
    *b = [0; 16];
    b[..2].copy_from_slice(&(libc::AF_INET as u16).to_ne_bytes());
    b[4..8].copy_from_slice(&a.octets());
}

#[cfg(target_os = "linux")]
fn set_sockaddr(s: &mut libc::sockaddr, a: Ipv4Addr) {
    let mut b = [0u8; 16];
    sockaddr_in(&mut b, a);
    unsafe { std::ptr::copy_nonoverlapping(b.as_ptr(), s as *mut libc::sockaddr as *mut u8, 16) };
}

/// The netmask of a prefix length (24 -> 255.255.255.0).
pub fn netmask(prefix: u8) -> Ipv4Addr {
    Ipv4Addr::from(if prefix == 0 { 0 } else { u32::MAX << (32 - prefix.min(32)) })
}

/// "ADDRESS/PREFIX" (a missing prefix: 24).
pub fn parse_cidr(s: &str) -> Option<(Ipv4Addr, Ipv4Addr)> {
    let (a, p) = s.split_once('/').unwrap_or((s, "24"));
    Some((a.parse().ok()?, netmask(p.parse().ok()?)))
}

/// The name servers, replacing the ones before.
pub fn set_name_servers(servers: &[Ipv4Addr]) -> io::Result<()> {
    let text: String = servers.iter().map(|s| format!("nameserver {s}\n")).collect();
    std::fs::write(RESOLV_CONF, text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn masks() {
        assert_eq!(netmask(24), Ipv4Addr::new(255, 255, 255, 0));
        assert_eq!(netmask(16), Ipv4Addr::new(255, 255, 0, 0));
        assert_eq!(parse_cidr("192.0.2.9/23"), Some((Ipv4Addr::new(192, 0, 2, 9), Ipv4Addr::new(255, 255, 254, 0))));
    }
}

/// The Wi-Fi supplicant's control socket: its status for the State record, scans.
pub mod wpa;

// the program (zd1-dhcp), in the multicall (module/os/apps/zd1)
#[cfg(target_os = "linux")]
mod cli;
#[cfg(target_os = "linux")]
pub use cli::main;
