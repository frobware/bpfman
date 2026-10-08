//! Small synchronous route-netlink inspection/deletion boundary.
//! Aya owns filter creation and reports its exact handle. This module supplies
//! the qdisc/filter dumps its public API lacks, using safe rustix sockets.
use rustix::net::{
    AddressFamily, RecvFlags, SendFlags, SocketFlags, SocketType, netlink::SocketAddrNetlink,
};
use std::io;

pub(super) const INGRESS: u32 = 0xfffffff2;
const EGRESS: u32 = 0xfffffff3;
const QDISC_PARENT: u32 = 0xfffffff1;
const QDISC_HANDLE: u32 = 0xffff0000;
const PRIORITY: u16 = 50;

#[derive(Debug)]
struct Message {
    kind: u16,
    flags: u16,
    body: Vec<u8>,
}

#[derive(Debug)]
struct Tc {
    handle: u32,
    parent: u32,
    info: u32,
    chain: u32,
    kind: String,
    program: Option<u32>,
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

fn u32_at(bytes: &[u8], start: usize) -> io::Result<u32> {
    Ok(u32::from_ne_bytes(
        bytes
            .get(start..start + 4)
            .ok_or_else(|| invalid("short netlink u32"))?
            .try_into()
            .map_err(|_| invalid("short netlink u32"))?,
    ))
}

fn u16_at(bytes: &[u8], start: usize) -> io::Result<u16> {
    Ok(u16::from_ne_bytes(
        bytes
            .get(start..start + 2)
            .ok_or_else(|| invalid("short netlink u16"))?
            .try_into()
            .map_err(|_| invalid("short netlink u16"))?,
    ))
}

fn aligned(n: usize) -> usize {
    (n + 3) & !3
}

fn attrs(mut bytes: &[u8]) -> io::Result<Vec<(u16, &[u8])>> {
    let mut values = Vec::new();
    while !bytes.is_empty() {
        let len = usize::from(u16_at(bytes, 0)?);
        if len < 4 || len > bytes.len() {
            return Err(invalid("invalid TC attribute length"));
        }
        values.push((u16_at(bytes, 2)? & 0x3fff, &bytes[4..len]));
        bytes = bytes
            .get(aligned(len)..)
            .ok_or_else(|| invalid("truncated TC attribute padding"))?;
    }
    Ok(values)
}

fn messages(mut bytes: &[u8]) -> io::Result<Vec<Message>> {
    let mut result = Vec::new();
    while !bytes.is_empty() {
        let len = u32_at(bytes, 0)? as usize;
        if len < 16 || len > bytes.len() {
            return Err(invalid("invalid netlink message length"));
        }
        if u32_at(bytes, 8)? != 1 {
            return Err(invalid("unexpected netlink sequence"));
        }
        result.push(Message {
            kind: u16_at(bytes, 4)?,
            flags: u16_at(bytes, 6)?,
            body: bytes[16..len].to_vec(),
        });
        bytes = bytes
            .get(aligned(len)..)
            .ok_or_else(|| invalid("truncated netlink message padding"))?;
    }
    Ok(result)
}

fn request(
    kind: u16,
    dump: bool,
    ifindex: u32,
    parent: u32,
    handle: u32,
    info: u32,
) -> io::Result<Vec<Tc>> {
    let socket = rustix::net::socket_with(
        AddressFamily::NETLINK,
        SocketType::RAW,
        SocketFlags::CLOEXEC,
        None,
    )?;
    let kernel = SocketAddrNetlink::new(0, 0);
    rustix::net::connect(&socket, &kernel)?;
    let mut bytes = Vec::with_capacity(36);
    bytes.extend_from_slice(&36u32.to_ne_bytes());
    bytes.extend_from_slice(&kind.to_ne_bytes());
    bytes.extend_from_slice(&(if dump { 0x301u16 } else { 5u16 }).to_ne_bytes());
    bytes.extend_from_slice(&1u32.to_ne_bytes());
    bytes.extend_from_slice(&0u32.to_ne_bytes());
    bytes.extend_from_slice(&0u32.to_ne_bytes()); // family and padding
    bytes.extend_from_slice(&ifindex.to_ne_bytes());
    bytes.extend_from_slice(&handle.to_ne_bytes());
    bytes.extend_from_slice(&parent.to_ne_bytes());
    bytes.extend_from_slice(&info.to_ne_bytes());
    if rustix::net::send(&socket, &bytes, SendFlags::empty())? != bytes.len() {
        return Err(invalid("short netlink send"));
    }
    let mut found = Vec::new();
    loop {
        let mut buffer = vec![0u8; 1024 * 1024];
        let (_, received, source) =
            rustix::net::recvfrom(&socket, buffer.as_mut_slice(), RecvFlags::TRUNC)?;
        let source = source
            .and_then(|source| SocketAddrNetlink::try_from(source).ok())
            .ok_or_else(|| invalid("missing netlink sender"))?;
        if source.pid() != 0 || source.groups() != 0 {
            return Err(invalid("unexpected netlink sender"));
        }
        // TRUNC asks Linux to return the full datagram length; never parse partial evidence.
        if received > buffer.len() {
            return Err(invalid("truncated netlink dump"));
        }
        for message in messages(&buffer[..received])? {
            if message.flags & 0x10 != 0 {
                return Err(invalid("interrupted netlink dump"));
            }
            match message.kind {
                1 => return Err(invalid("unexpected netlink NOOP")),
                2 => {
                    let errno = u32_at(&message.body, 0)? as i32;
                    if errno != 0 {
                        return Err(io::Error::from_raw_os_error(-errno));
                    }
                    if !dump {
                        return Ok(found);
                    }
                }
                3 => {
                    if message.body.len() >= 4 && u32_at(&message.body, 0)? != 0 {
                        return Err(invalid("failed netlink dump"));
                    }
                    return Ok(found);
                }
                16..=51 => {
                    if message.kind != (if kind == 38 { 36 } else { 44 }) {
                        return Err(invalid("unexpected netlink TC response"));
                    }
                    if u32_at(&message.body, 4)? != ifindex {
                        continue;
                    }
                    let mut tc = Tc {
                        handle: u32_at(&message.body, 8)?,
                        parent: u32_at(&message.body, 12)?,
                        info: u32_at(&message.body, 16)?,
                        chain: 0,
                        kind: String::new(),
                        program: None,
                    };
                    for (attribute, value) in attrs(
                        message
                            .body
                            .get(20..)
                            .ok_or_else(|| invalid("short TC message"))?,
                    )? {
                        match attribute {
                            1 => {
                                let name = value
                                    .strip_suffix(&[0])
                                    .ok_or_else(|| invalid("unterminated TC kind"))?;
                                tc.kind = std::str::from_utf8(name)
                                    .map_err(|_| invalid("invalid TC kind"))?
                                    .into();
                            }
                            2 if kind == 46 => {
                                // TCA_OPTIONS contains the classifier-specific TCA_BPF_ID.
                                for (kind, value) in attrs(value)? {
                                    if kind == 11 {
                                        tc.program = Some(u32_at(value, 0)?);
                                    }
                                }
                            }
                            11 => tc.chain = u32_at(value, 0)?, // Top-level TCA_CHAIN.
                            _ => {}
                        }
                    }
                    found.push(tc);
                }
                _ => return Err(invalid("unexpected netlink response")),
            }
        }
    }
}

/// Inspect the ingress qdisc slot. A classic ingress qdisc is a conflict.
pub(super) fn clsact(ifindex: u32) -> io::Result<bool> {
    let qdiscs = request(38, true, ifindex, 0, 0, 0)?;
    let mut found = false;
    for q in qdiscs {
        if q.kind == "clsact" {
            found = true;
        } else if q.parent == QDISC_PARENT {
            return Err(invalid("a non-clsact qdisc occupies the ingress slot"));
        }
    }
    Ok(found)
}

/// Validate only the exact stored filter; absence is successful teardown evidence.
pub(super) fn filter(ifindex: u32, handle: u32, dispatcher: u32) -> io::Result<bool> {
    for f in request(46, true, ifindex, INGRESS, 0, 0)? {
        if f.parent == INGRESS
            && f.chain == 0
            && (f.info >> 16) == u32::from(PRIORITY)
            && (f.info & 0xffff) == u32::from(3u16.to_be())
            && f.handle == handle
        {
            if f.kind != "bpf" || f.program != Some(dispatcher) {
                return Err(invalid("TC filter identity differs from snapshot"));
            }
            return Ok(true);
        }
    }
    Ok(false)
}

pub(super) fn detach(ifindex: u32, handle: u32, dispatcher: u32) -> io::Result<()> {
    if !filter(ifindex, handle, dispatcher)? {
        return Ok(());
    }
    // ETH_P_ALL is encoded in network byte order within the native-endian info word.
    let info = (u32::from(PRIORITY) << 16) | u32::from(3u16.to_be());
    match request(45, false, ifindex, INGRESS, handle, info) {
        Ok(_) => Ok(()),
        Err(e) if e.raw_os_error() == Some(libc::ENOENT) => Ok(()),
        Err(e) => Err(e),
    }
}

pub(super) fn reclaim_clsact(ifindex: u32) -> io::Result<()> {
    if !clsact(ifindex)? {
        return Ok(());
    }
    for parent in [INGRESS, EGRESS] {
        if !request(46, true, ifindex, parent, 0, 0)?.is_empty() {
            return Ok(());
        }
    }
    match request(37, false, ifindex, QDISC_PARENT, QDISC_HANDLE, 0) {
        Ok(_) => Ok(()),
        Err(e) if e.raw_os_error() == Some(libc::ENOENT) => Ok(()),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncated_or_malformed_evidence_is_rejected() {
        for bytes in [vec![], vec![0; 3], vec![0; 15], vec![0; 16]] {
            if !bytes.is_empty() {
                assert!(messages(&bytes).is_err());
            }
        }
        assert!(attrs(&[3, 0, 1, 0]).is_err());
        assert!(attrs(&[8, 0, 1, 0, 0, 0, 0]).is_err());
    }
}
