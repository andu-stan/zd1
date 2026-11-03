//! zd1-dhcp: what udhcpc runs on each of its events (`udhcpc -s /bin/zd1-dhcp`): the event as the argument, the lease
//! in the environment (interface, ip, subnet, router, dns).
//!   bound | renew   the address, the default route through the first router, the name servers; a new address
//!                   restarts the AirPlay receiver (it advertises on the addresses it finds at start)
//!   deconfig        the address dropped, except on an interface with a link-local fallback
//!   leasefail       an interface with a fallback and no address gets it
//! eth0's fallback is 169.254.1.1/16, like the stock module's: a Mac on the wired link (no DHCP server) always reaches
//! it (docs/module/warp.md section 7).

use crate::{Iface, netmask, set_name_servers};
use std::net::Ipv4Addr;

/// Interfaces that keep a link-local address when no server answers.
const LINK_LOCAL: &[(&str, Ipv4Addr, u8)] = &[("eth0", Ipv4Addr::new(169, 254, 1, 1), 16)];

fn say(s: &str) {
    zd1_sys::kmsg("[dhcp]", s);
}

fn env(name: &str) -> String {
    std::env::var(name).unwrap_or_default()
}

fn addrs(name: &str) -> Vec<Ipv4Addr> {
    env(name).split_whitespace().filter_map(|a| a.parse().ok()).collect()
}

pub fn main() {
    let event = std::env::args().nth(1).unwrap_or_default();
    let name = env("interface");
    let iface = match Iface::open(&name) {
        Ok(i) => i,
        Err(e) => {
            say(&format!("{name}: {e}"));
            std::process::exit(1);
        }
    };
    let fallback = LINK_LOCAL.iter().find(|(n, _, _)| *n == name);
    match event.as_str() {
        "bound" | "renew" => {
            let Ok(ip) = env("ip").parse::<Ipv4Addr>() else {
                say(&format!("{name} {event}: no address"));
                return;
            };
            let mask = env("subnet").parse().unwrap_or(netmask(24));
            let before = iface.ipv4();
            if let Err(e) = iface.set_ipv4(ip, mask) {
                say(&format!("{name} {ip}: {e}"));
                std::process::exit(1);
            }
            let routers = addrs("router");
            if let Some(&gw) = routers.first() {
                iface.remove_default_routes();
                if let Err(e) = iface.add_default_route(gw) {
                    say(&format!("{name} router {gw}: {e}"));
                }
            }
            let dns = addrs("dns");
            if !dns.is_empty() {
                let _ = set_name_servers(&dns);
            }
            let gw = routers.first().map(|g| format!(" router {g}")).unwrap_or_default();
            say(&format!("{name} {event} {ip}{gw}"));
            if before != Some(ip) {
                // its mDNS records carry the old address (none, or the link-local one before the lease)
                say(&format!("{name}: a new address: restarting airplay"));
                let _ = zd1_svc::request("restart airplay");
            }
        }
        "deconfig" if fallback.is_none() => {
            let _ = iface.set_ipv4(Ipv4Addr::UNSPECIFIED, Ipv4Addr::UNSPECIFIED);
        }
        "leasefail" => {
            if let Some(&(_, ip, prefix)) = fallback
                && iface.ipv4().is_none()
            {
                let _ = iface.set_ipv4(ip, netmask(prefix));
                say(&format!("{name}: no lease, link-local {ip}"));
            }
        }
        _ => {}
    }
}
