//! UDP multicast shred receiver.
//!
//! Binds to a UDP socket, joins the DoubleZero multicast group, and emits
//! raw shred bytes with a nanosecond receive timestamp. Designed to run on
//! a CPU-pinned, isolated core with `SO_BUSY_POLL` for minimum latency.

use anyhow::Result;
use crossbeam_channel::Sender;
use socket2::{Domain, Protocol, Socket, Type};
use std::mem::MaybeUninit;
use std::net::{Ipv4Addr, SocketAddrV4};
use std::sync::Arc;

use crate::metrics;
use crate::source_metrics::SourceMetrics;

/// Raw shred bytes received from UDP multicast
pub struct RawShred {
    pub data: Vec<u8>,
    pub recv_timestamp_ns: u64,
}

pub struct ShredReceiver {
    socket: Socket,
    tx: Sender<RawShred>,
    buf: Vec<u8>,
    metrics: Arc<SourceMetrics>,
}

impl ShredReceiver {
    /// Bind to the multicast group on the specified interface.
    pub fn new(
        multicast_addr: &str,
        port: u16,
        interface: &str,
        tx: Sender<RawShred>,
        metrics: Arc<SourceMetrics>,
    ) -> Result<Self> {
        let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
        socket.set_reuse_address(true)?;
        #[cfg(target_os = "linux")]
        {
            use std::os::unix::io::AsRawFd;
            unsafe {
                let val: libc::c_int = 1;
                libc::setsockopt(
                    socket.as_raw_fd(),
                    libc::SOL_SOCKET,
                    libc::SO_REUSEPORT,
                    &val as *const _ as *const libc::c_void,
                    std::mem::size_of::<libc::c_int>() as libc::socklen_t,
                );
            }
        }

        let bind_addr = SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, port);
        socket.bind(&bind_addr.into())?;

        // Join multicast group on the specified interface
        let mcast_addr: Ipv4Addr = multicast_addr.parse()?;
        let iface_addr = Self::resolve_interface_addr(interface)?;
        socket.join_multicast_v4(&mcast_addr, &iface_addr)?;

        // Enable busy-poll for lower latency
        #[cfg(target_os = "linux")]
        {
            use std::os::unix::io::AsRawFd;
            unsafe {
                let val: libc::c_int = 50; // 50µs busy poll
                libc::setsockopt(
                    socket.as_raw_fd(),
                    libc::SOL_SOCKET,
                    libc::SO_BUSY_POLL,
                    &val as *const _ as *const libc::c_void,
                    std::mem::size_of::<libc::c_int>() as libc::socklen_t,
                );
            }
        }

        // 4MB receive buffer
        socket.set_recv_buffer_size(4 * 1024 * 1024)?;

        Ok(Self {
            socket,
            tx,
            buf: vec![0u8; 1500], // MTU-sized buffer
            metrics,
        })
    }

    /// Main receive loop — should run on a pinned, isolated core
    pub fn run(&mut self) -> Result<()> {
        tracing::info!("shred receiver started");
        loop {
            // SAFETY: we're reading into a pre-allocated buffer
            let buf_uninit: &mut [MaybeUninit<u8>] = unsafe {
                std::slice::from_raw_parts_mut(
                    self.buf.as_mut_ptr() as *mut MaybeUninit<u8>,
                    self.buf.len(),
                )
            };
            let n = self.socket.recv(buf_uninit)?;

            let ts = metrics::now_ns();

            if n > 0 {
                self.metrics
                    .shreds_received
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                self.metrics
                    .bytes_received
                    .fetch_add(n as u64, std::sync::atomic::Ordering::Relaxed);
                let shred = RawShred {
                    data: self.buf[..n].to_vec(),
                    recv_timestamp_ns: ts,
                };
                // Non-blocking send — drop if the decoder channel is full (backpressure).
                // Dropped shreds mean the decoder is falling behind; visible via shreds_dropped.
                if self.tx.try_send(shred).is_err() {
                    self.metrics
                        .shreds_dropped
                        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
            }
        }
    }

    /// Resolve a network interface name to its IPv4 address
    fn resolve_interface_addr(interface: &str) -> Result<Ipv4Addr> {
        #[cfg(target_os = "linux")]
        {
            use std::ffi::CStr;
            unsafe {
                let mut addrs: *mut libc::ifaddrs = std::ptr::null_mut();
                if libc::getifaddrs(&mut addrs) != 0 {
                    anyhow::bail!("getifaddrs failed");
                }

                let mut current = addrs;
                while !current.is_null() {
                    let ifa = &*current;
                    if !ifa.ifa_name.is_null() && !ifa.ifa_addr.is_null() {
                        let name = CStr::from_ptr(ifa.ifa_name).to_str().unwrap_or("");
                        if name == interface
                            && (*ifa.ifa_addr).sa_family == libc::AF_INET as libc::sa_family_t
                        {
                            let sockaddr_in = &*(ifa.ifa_addr as *const libc::sockaddr_in);
                            let ip = Ipv4Addr::from(u32::from_be(sockaddr_in.sin_addr.s_addr));
                            libc::freeifaddrs(addrs);
                            return Ok(ip);
                        }
                    }
                    current = ifa.ifa_next;
                }
                libc::freeifaddrs(addrs);
            }
            anyhow::bail!("interface {} not found", interface);
        }

        #[cfg(not(target_os = "linux"))]
        {
            let _ = interface;
            Ok(Ipv4Addr::LOCALHOST)
        }
    }
}
