//! Private, synchronous mount-namespace target handoff. No BPF or store effects.
use anyhow::Context;
use bpfman_runtime::Cancellation;
use clap::builder::{PathBufValueParser, TypedValueParser};
use rustix::net::{
    self, RecvAncillaryBuffer, RecvAncillaryMessage, RecvFlags, ReturnFlags, SendAncillaryBuffer,
    SendAncillaryMessage, SendFlags,
};
use std::{
    ffi::OsStr,
    fs::{File, OpenOptions},
    io::{self, IoSlice, IoSliceMut},
    mem::MaybeUninit,
    os::{
        fd::{AsFd, OwnedFd},
        unix::{
            ffi::OsStrExt,
            fs::{MetadataExt, OpenOptionsExt},
            net::UnixDatagram,
        },
    },
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

#[path = "ns_helper/inherited.rs"]
mod inherited;

#[derive(clap::Subcommand)]
pub(crate) enum Helper {
    /// Resolve and open one regular-file target in an inherited mount namespace.
    OpenUprobeTarget {
        #[arg(value_parser = PathBufValueParser::new().try_map(helper_target))]
        target: PathBuf,
        #[arg(long, value_parser = clap::value_parser!(i32).range(3..))]
        socket_fd: i32,
        #[arg(long, value_parser = clap::value_parser!(i32).range(3..))]
        namespace_fd: i32,
    },
}

fn helper_target(path: PathBuf) -> Result<PathBuf, String> {
    if path.is_absolute() && !path.as_os_str().as_bytes().ends_with(b"/") {
        return Ok(path);
    }
    let name = path
        .to_str()
        .ok_or("expected an absolute path or bare library name")?;
    let target =
        bpfman_model::UprobeTarget::new(name, None, 1, 0).map_err(|error| error.to_string())?;
    if !target.is_library() {
        return Err("expected an absolute path or bare library name".into());
    }
    Ok(path)
}

fn cache_target(target: &Path) -> io::Result<PathBuf> {
    if target.is_absolute() {
        return Ok(target.to_owned());
    }
    let name = target
        .to_str()
        .ok_or_else(|| io::Error::other("invalid library name"))?;
    let bytes = resolution_bytes(Path::new("/etc/ld.so.cache"))?;
    let path = bpfman_core::uprobe_library_from_cache(&bytes, name)
        .map_err(io::Error::other)?
        .ok_or_else(|| {
            io::Error::other(format!("library {name:?} not found in /etc/ld.so.cache"))
        })?;
    Ok(PathBuf::from(OsStr::from_bytes(path)))
}

impl Helper {
    // Called once, before signal handling or other thread creation.
    pub(super) fn execute(self) -> anyhow::Result<()> {
        let Self::OpenUprobeTarget {
            target,
            socket_fd,
            namespace_fd,
        } = self;
        let lock_fd: i32 = std::env::var(bpfman_lock::WRITER_LOCK_FD_ENV)?.parse()?;
        if lock_fd < 3
            || lock_fd == socket_fd
            || lock_fd == namespace_fd
            || socket_fd == namespace_fd
        {
            anyhow::bail!("helper descriptors must be distinct and above standard I/O");
        }
        let socket = inherited::take(socket_fd)?;
        let namespace = inherited::take(namespace_fd)?;
        let lock = File::from(inherited::take(lock_fd)?);
        let metadata = lock.metadata()?;
        if !metadata.is_file() || metadata.nlink() != 1 {
            anyhow::bail!("helper requires a singly linked regular writer-lock file");
        }
        if net::sockopt::socket_type(&socket)? != net::SocketType::DGRAM {
            anyhow::bail!("helper requires a Unix datagram socket");
        }
        // Verify the descriptor, never acquire the runtime lock by pathname.
        let _lock = bpfman_lock::InheritedWriteLock::from_fd(lock.into())?;
        rustix::thread::move_into_link_name_space(
            namespace.as_fd(),
            Some(rustix::thread::LinkNameSpaceType::Mount),
        )?;
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(rustix::fs::OFlags::NONBLOCK.bits() as i32)
            .open(cache_target(&target)?)?;
        require_regular(&file)?;
        send_target(&socket, &file)?;
        Ok(())
    }
}

pub(super) struct Launcher {
    executable: PathBuf,
    cancellation: Cancellation,
}

impl Launcher {
    pub(super) fn new(executable: PathBuf, cancellation: Cancellation) -> Self {
        Self {
            executable,
            cancellation,
        }
    }

    pub(super) fn open(
        &self,
        writer: &bpfman_fs::RuntimeWriter<'_>,
        target: &bpfman_model::UprobeLink,
    ) -> io::Result<File> {
        self.open_with_cancellation(writer, target, self.cancellation.flag())
    }

    pub(super) fn open_with_cancellation(
        &self,
        writer: &bpfman_fs::RuntimeWriter<'_>,
        target: &bpfman_model::UprobeLink,
        cancelled_flag: &std::sync::atomic::AtomicBool,
    ) -> io::Result<File> {
        let pid = target
            .container_pid()
            .ok_or_else(|| io::Error::other("missing container PID"))?;
        let (namespace, proc_root) = open_namespace(pid.get())?;
        // Host PID maps must be interpreted before entering a container's mount
        // namespace, whose procfs can use a different PID numbering.
        let mapped = if let Some(pid) = target.pid() {
            let bytes = resolution_bytes(&proc_root.join(pid.to_string()).join("maps"))?;
            bpfman_core::uprobe_library_from_maps(&bytes, target.target())
                .map(|path| PathBuf::from(OsStr::from_bytes(path)))
        } else {
            None
        };
        let lookup = mapped.unwrap_or_else(|| target.target().into());
        let lock = writer.duplicate_lock_fd().map_err(io::Error::other)?;
        self.launch_with_cancellation(&lookup, namespace, lock, cancelled_flag)
    }

    fn launch_with_cancellation(
        &self,
        target: &Path,
        namespace: File,
        lock: OwnedFd,
        cancelled_flag: &std::sync::atomic::AtomicBool,
    ) -> io::Result<File> {
        helper_target(target.to_owned()).map_err(io::Error::other)?;
        if self.cancellation.is_cancelled()
            || cancelled_flag.load(std::sync::atomic::Ordering::Acquire)
        {
            return Err(cancelled());
        }
        let (parent, child_socket) = UnixDatagram::pair()?;
        let mut command = Command::new(&self.executable);
        command
            .args([
                "__ns-helper",
                "open-uprobe-target",
                "--socket-fd",
                "3",
                "--namespace-fd",
                "5",
                "--",
            ])
            .arg(target)
            .env(bpfman_lock::WRITER_LOCK_FD_ENV, "4")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit());
        inherited::configure(
            &mut command,
            child_socket.as_fd(),
            lock.as_fd(),
            namespace.as_fd(),
        )?;
        let mut child = Reap(command.spawn()?);
        // configure owns only CLOEXEC duplicates. All parent-side copies of the
        // child's socket/namespace/lock close before waiting or attachment.
        drop(command);
        drop(child_socket);
        drop(namespace);
        drop(lock);
        let started = Instant::now();
        loop {
            if self.cancellation.is_cancelled()
                || cancelled_flag.load(std::sync::atomic::Ordering::Acquire)
            {
                return Err(cancelled());
            }
            if let Some(status) = child.0.try_wait()? {
                if !status.success() {
                    return Err(io::Error::other(format!(
                        "mount-namespace helper exited with {status}"
                    )));
                }
                parent.set_nonblocking(true)?;
                let file = receive_target(&parent)?;
                require_regular(&file)?;
                return Ok(file);
            }
            if started.elapsed() >= Duration::from_secs(30) {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "mount-namespace helper timed out",
                ));
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

fn open_namespace(pid: u32) -> io::Result<(File, PathBuf)> {
    match File::open(format!("/proc/{pid}/ns/mnt")) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            File::open(format!("/host/proc/{pid}/ns/mnt")).map(|file| (file, "/host/proc".into()))
        }
        result => result.map(|file| (file, "/proc".into())),
    }
}

fn require_regular(file: &File) -> io::Result<()> {
    if !file.metadata()?.is_file() {
        return Err(io::Error::other("uprobe target must be a regular file"));
    }
    Ok(())
}

fn cancelled() -> io::Error {
    io::Error::new(
        io::ErrorKind::Interrupted,
        "container target lookup cancelled",
    )
}

struct Reap(Child);

impl Drop for Reap {
    fn drop(&mut self) {
        // kill handles a child blocked before handoff. wait reaps on every path,
        // including malformed messages and a successful try_wait above.
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn send_target(socket: impl AsFd, file: &File) -> io::Result<()> {
    let mut space = [MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(1))];
    let mut ancillary = SendAncillaryBuffer::new(&mut space);
    let fds = [file.as_fd()];
    if !ancillary.push(SendAncillaryMessage::ScmRights(&fds)) {
        return Err(io::Error::other("descriptor message exceeds buffer"));
    }
    if net::sendmsg(
        socket,
        &[IoSlice::new(b"U")],
        &mut ancillary,
        SendFlags::NOSIGNAL,
    )? != 1
    {
        return Err(io::Error::other("incomplete target descriptor message"));
    }
    Ok(())
}

fn receive_target(socket: impl AsFd) -> io::Result<File> {
    let mut space = [MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(2))];
    let mut ancillary = RecvAncillaryBuffer::new(&mut space);
    let mut byte = [0; 2];
    let message = net::recvmsg(
        socket,
        &mut [IoSliceMut::new(&mut byte)],
        &mut ancillary,
        RecvFlags::CMSG_CLOEXEC | RecvFlags::DONTWAIT,
    )?;
    let mut fds = Vec::new();
    let mut unexpected = false;
    for message in ancillary.drain() {
        match message {
            RecvAncillaryMessage::ScmRights(rights) => fds.extend(rights),
            _ => unexpected = true,
        }
    }
    if unexpected
        || message
            .flags
            .intersects(ReturnFlags::CTRUNC | ReturnFlags::TRUNC)
        || message.bytes != 1
        || byte[0] != b'U'
        || fds.len() != 1
    {
        return Err(io::Error::other("invalid target descriptor message"));
    }
    let fd = fds
        .pop()
        .ok_or_else(|| io::Error::other("missing target descriptor"))?;
    Ok(File::from(fd))
}

fn resolution_bytes(path: &Path) -> io::Result<Vec<u8>> {
    std::fs::read(path)
        .with_context(|| {
            format!(
                "read user-space target resolution data at {}",
                path.display()
            )
        })
        .map_err(io::Error::other)
}
