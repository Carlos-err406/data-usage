//! Per-interface byte counters, read from the kernel's interface MIB.
//!
//! Two dead ends worth recording, since both look correct and aren't:
//!
//!   * `getifaddrs()` returns `struct if_data`, whose ifi_ibytes/ifi_obytes are
//!     32-bit and wrap every 4 GB.
//!   * `sysctl(NET_RT_IFLIST2)` returns `struct if_data64`, which *should* be
//!     64-bit — but the kernel leaves the high words zeroed on this path, so
//!     the counters silently wrap at 4 GB too.
//!
//! `net.link.generic.ifdata.<index>.general` is the one that actually carries
//! full 64-bit counters; it's what `netstat -ib` agrees with.
//!
//! The headers wrap these structs in `#pragma pack(4)`, which lands `if_data64`
//! at offset 52 of `ifmibdata` — 4-byte aligned, not 8. A `#[repr(C)]` struct
//! would re-align it to 56 and read garbage, so we index by explicit offset.

use std::io;

const CTL_NET: libc::c_int = 4;
const PF_LINK: libc::c_int = 18;
const NETLINK_GENERIC: libc::c_int = 0;
const IFMIB_SYSTEM: libc::c_int = 1;
const IFMIB_IFDATA: libc::c_int = 2;
const IFMIB_IFCOUNT: libc::c_int = 1;
const IFDATA_GENERAL: libc::c_int = 1;

const NAME_LEN: usize = 16;
const OFF_DATA: usize = 52;
const OFF_IBYTES: usize = OFF_DATA + 64;
const OFF_OBYTES: usize = OFF_DATA + 72;
const IFMIBDATA_LEN: usize = OFF_DATA + 128;

/// A snapshot of one interface's lifetime byte counters.
#[derive(Debug, Clone)]
pub struct IfCounters {
    pub name: String,
    pub rx: u64,
    pub tx: u64,
}

fn sysctl_into(mib: &mut [libc::c_int], buf: &mut [u8]) -> io::Result<usize> {
    let mut len: libc::size_t = buf.len();
    let rc = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            mib.len() as libc::c_uint,
            buf.as_mut_ptr() as *mut libc::c_void,
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(len)
}

/// Highest interface index currently allocated by the kernel.
fn interface_count() -> io::Result<u32> {
    let mut mib = [
        CTL_NET,
        PF_LINK,
        NETLINK_GENERIC,
        IFMIB_SYSTEM,
        IFMIB_IFCOUNT,
    ];
    let mut buf = [0u8; 4];
    sysctl_into(&mut mib, &mut buf)?;
    Ok(u32::from_ne_bytes(buf))
}

fn read_one(index: u32) -> io::Result<IfCounters> {
    let mut mib = [
        CTL_NET,
        PF_LINK,
        NETLINK_GENERIC,
        IFMIB_IFDATA,
        index as libc::c_int,
        IFDATA_GENERAL,
    ];
    let mut buf = [0u8; IFMIBDATA_LEN];
    let n = sysctl_into(&mut mib, &mut buf)?;
    if n < IFMIBDATA_LEN {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "short ifmibdata",
        ));
    }

    let name_end = buf[..NAME_LEN]
        .iter()
        .position(|&b| b == 0)
        .unwrap_or(NAME_LEN);
    let name = String::from_utf8_lossy(&buf[..name_end]).into_owned();
    let u64_at = |o: usize| u64::from_ne_bytes(buf[o..o + 8].try_into().unwrap());

    Ok(IfCounters {
        name,
        rx: u64_at(OFF_IBYTES),
        tx: u64_at(OFF_OBYTES),
    })
}

/// Snapshot every interface. Cheap enough to call once a second.
pub fn read() -> io::Result<Vec<IfCounters>> {
    let count = interface_count()?;
    let mut out = Vec::with_capacity(count as usize);
    for index in 1..=count {
        // Indices can have holes as interfaces come and go; skip rather than fail.
        if let Ok(c) = read_one(index)
            && !c.name.is_empty()
        {
            out.push(c);
        }
    }
    Ok(out)
}
