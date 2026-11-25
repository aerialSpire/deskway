//! The default IPv4 route on the interface that owns a session socket's address.

use std::net::Ipv4Addr;

#[cfg(any(windows, target_os = "macos"))]
pub fn default_gateway(local: Ipv4Addr) -> Option<Ipv4Addr> {
    if !local.is_private() {
        return None;
    }
    let index = if_addrs::get_if_addrs().ok()?.into_iter().find(|i| i.ip() == local)?.index?;
    gateway_on(index)
}

#[cfg(not(any(windows, target_os = "macos")))]
pub fn default_gateway(_: Ipv4Addr) -> Option<Ipv4Addr> {
    None
}

#[cfg(windows)]
fn gateway_on(index: u32) -> Option<Ipv4Addr> {
    use std::{ptr, slice};
    use windows::Win32::NetworkManagement::IpHelper::{FreeMibTable, GetIpForwardTable2};
    use windows::Win32::Networking::WinSock::AF_INET;

    let mut table = ptr::null_mut();
    if unsafe { GetIpForwardTable2(AF_INET, &mut table) }.is_err() || table.is_null() {
        return None;
    }
    // IP Helper owns this variable-length table until FreeMibTable. Table is its first row.
    unsafe {
        let rows = slice::from_raw_parts(ptr::addr_of!((*table).Table).cast::<_>(), (*table).NumEntries as usize);
        let gateway = rows
            .iter()
            .filter_map(|row: &windows::Win32::NetworkManagement::IpHelper::MIB_IPFORWARD_ROW2| {
                if row.InterfaceIndex != index
                    || row.DestinationPrefix.PrefixLength != 0
                    || row.NextHop.si_family != AF_INET
                {
                    return None;
                }
                let gateway = Ipv4Addr::from(row.NextHop.Ipv4.sin_addr.S_un.S_addr.to_ne_bytes());
                (!gateway.is_unspecified()).then_some((row.Metric, gateway))
            })
            .min_by_key(|(metric, _)| *metric)
            .map(|(_, gateway)| gateway);
        FreeMibTable(table.cast());
        gateway
    }
}

#[cfg(target_os = "macos")]
fn gateway_on(index: u32) -> Option<Ipv4Addr> {
    use libc::{
        rt_msghdr2, sockaddr_in, sysctl, AF_INET, CTL_NET, NET_RT_DUMP2, PF_ROUTE, RTA_DST,
        RTA_GATEWAY, RTA_NETMASK, RTF_GATEWAY, RTF_HOST, RTF_UP, RTM_GET2, RTM_VERSION,
    };
    use std::ptr::{null_mut, read_unaligned};

    let mut mib = [CTL_NET, PF_ROUTE, 0, AF_INET, NET_RT_DUMP2, 0];
    let mut len = 0;
    let mut dump = |out, len| unsafe { sysctl(mib.as_mut_ptr(), mib.len() as _, out, len, null_mut(), 0) == 0 };
    if !dump(null_mut(), &mut len) {
        return None;
    }
    let mut buf = vec![0u8; len];
    if !dump(buf.as_mut_ptr().cast(), &mut len) {
        return None;
    }
    let mut routes = buf.get(..len)?;
    let size = std::mem::size_of::<rt_msghdr2>();
    let sa_size = std::mem::size_of::<sockaddr_in>();
    while routes.len() >= size {
        // Route messages are four-byte aligned. The Vec's byte buffer need not be aligned.
        let header = unsafe { read_unaligned(routes.as_ptr().cast::<rt_msghdr2>()) };
        let len = usize::from(header.rtm_msglen);
        if len < size {
            return None;
        }
        let record = routes.get(size..len)?;
        routes = &routes[len..];
        if i32::from(header.rtm_version) != RTM_VERSION
            || i32::from(header.rtm_type) != RTM_GET2
            || u32::from(header.rtm_index) != index
            || header.rtm_flags & (RTF_UP | RTF_GATEWAY | RTF_HOST) != (RTF_UP | RTF_GATEWAY)
            || header.rtm_addrs & (RTA_DST | RTA_GATEWAY) != (RTA_DST | RTA_GATEWAY)
            || record.len() < 2 * sa_size
        {
            continue;
        }
        // Destination and next hop are the first two addresses. IPv4 sockaddrs are 16 bytes.
        let dst = unsafe { read_unaligned(record.as_ptr().cast::<sockaddr_in>()) };
        let next = unsafe { read_unaligned(record[sa_size..].as_ptr().cast::<sockaddr_in>()) };
        if i32::from(dst.sin_family) != AF_INET
            || i32::from(next.sin_family) != AF_INET
            || usize::from(dst.sin_len) != sa_size
            || usize::from(next.sin_len) != sa_size
            || dst.sin_addr.s_addr != 0
            || next.sin_addr.s_addr == 0
        {
            continue;
        }
        if header.rtm_addrs & RTA_NETMASK != 0 {
            // Darwin can omit trailing zeros in a netmask sockaddr, including a zero length.
            let mask = record.get(2 * sa_size..)?;
            let mask = mask.get(..usize::from(*mask.first()?))?;
            if mask.get(4..).is_some_and(|addr| addr.iter().any(|&b| b != 0)) {
                continue;
            }
        }
        return Some(next.sin_addr.s_addr.to_ne_bytes().into());
    }
    None
}
