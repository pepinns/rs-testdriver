use std::net::Ipv4Addr;
use std::sync::atomic;
use std::sync::atomic::AtomicU32;

pub struct FreeIp {
    cur_offset: AtomicU32,
    base_addr: Ipv4Addr,
    used_ports: Vec<u16>,
}
impl FreeIp {
    pub fn new(base_addr: Option<Ipv4Addr>, used_ports: Option<Vec<u16>>) -> Self {
        let base_addr = base_addr.unwrap_or(Ipv4Addr::new(127, 0, 0, 10));
        let used_ports = used_ports.unwrap_or_default();
        Self {
            cur_offset: AtomicU32::new(1),
            base_addr,
            used_ports,
        }
    }
    pub fn next_addr(&self) -> Ipv4Addr {
        let mut idx: u32 = self.base_addr.into();
        loop {
            let cur_offset = self.cur_offset.fetch_add(1, atomic::Ordering::SeqCst);
            let new_addr = idx + cur_offset;
            let next: Ipv4Addr = new_addr.into();

            if check_ports_in_use(&next, &self.used_ports) {
                idx += 1;
                tracing::debug!("Skipping used address {}", next);
                continue;
            } else {
                tracing::debug!("Chose address {}", next);
                return next;
            }
        }
    }
}
pub fn check_ports_in_use(ip: &Ipv4Addr, used_ports: &Vec<u16>) -> bool {
    for port in used_ports {
        if is_port_in_use(ip, *port) {
            return true;
        }
    }
    return false;
}
pub fn is_port_in_use(ip: &Ipv4Addr, port: u16) -> bool {
    match std::net::TcpListener::bind((ip.to_string(), port)) {
        Ok(listener) => {
            drop(listener);
            false
        }
        Err(e) => match e.kind() {
            std::io::ErrorKind::AddrInUse => true,
            _ => panic!("Failed to bind to port {}:{} e={:?}", ip, port, e),
        },
    }
}
