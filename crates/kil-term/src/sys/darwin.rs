//! Darwin FFI: PTYs, process inspection, signals (DESIGN.md §4, §10).
#![expect(
    unsafe_code,
    reason = "Darwin FFI; every block carries a SAFETY comment"
)]

use std::{
    ffi::{CStr, OsStr, OsString, c_char, c_int, c_void},
    io,
    mem::{MaybeUninit, size_of},
    os::{
        fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, OwnedFd, RawFd},
        unix::{ffi::OsStrExt, process::CommandExt},
    },
    path::PathBuf,
    process::{Child, Command},
    ptr,
    time::Duration,
};

use polling::{
    PollMode, Poller,
    os::kqueue::{PollerKqueueExt, Process, ProcessOps, Timer},
};

/// `SZOMB` from `<sys/proc.h>`: exited but not yet reaped.
const SZOMB: u32 = 5;

/// XNU's internal `EREDRIVEOPEN`: "redo this open". Concurrent opens of `/dev/ptmx` on macOS
/// 27 occasionally leak it to user space instead of retrying in the kernel (measured).
const EREDRIVEOPEN: i32 = -6;
const PTMX_ATTEMPTS: usize = 16;

/// Signal dispositions reset to default in the child, on top of std resetting the mask and
/// `SIGPIPE`: the GUI parent may have ignored or caught any of these.
const RESET_SIGNALS: [c_int; 10] = [
    libc::SIGCHLD,
    libc::SIGHUP,
    libc::SIGINT,
    libc::SIGQUIT,
    libc::SIGTERM,
    libc::SIGALRM,
    libc::SIGPIPE,
    libc::SIGTSTP,
    libc::SIGTTIN,
    libc::SIGTTOU,
];

fn cvt(rc: c_int) -> io::Result<c_int> {
    if rc == -1 {
        Err(io::Error::last_os_error())
    } else {
        Ok(rc)
    }
}

/// Opens a PTY pair sized `size`. Both ends are close-on-exec; the master is non-blocking.
///
/// Not libc's `openpty(3)`: it resolves the slave through `ptsname(3)`'s static buffer, so
/// concurrent spawns race and can even open each other's slave. Every step here is
/// thread-safe, and the slave is opened close-on-exec atomically.
pub(crate) fn openpty(size: libc::winsize) -> io::Result<(OwnedFd, OwnedFd)> {
    let master = open_ptmx()?;
    // A concurrent fork may inherit the master until this runs; every child we spawn sweeps
    // CLOEXEC over all descriptors before exec, so nothing leaks past exec.
    set_fd_flag(master.as_fd(), libc::FD_CLOEXEC)?;
    let mut name: [c_char; 128] = [0; 128];
    // SAFETY: grantpt/unlockpt act on a valid master; TIOCPTYGNAME writes a NUL-terminated
    // path of at most 128 bytes into `name`.
    unsafe {
        cvt(libc::grantpt(master.as_raw_fd()))?;
        cvt(libc::unlockpt(master.as_raw_fd()))?;
        cvt(libc::ioctl(
            master.as_raw_fd(),
            libc::TIOCPTYGNAME.into(),
            name.as_mut_ptr(),
        ))?;
    }
    let flags = libc::O_RDWR | libc::O_NOCTTY | libc::O_CLOEXEC;
    // SAFETY: `name` is NUL-terminated (zero-initialized, kernel-filled).
    let slave = cvt(unsafe { libc::open(name.as_ptr(), flags) })?;
    // SAFETY: just opened above and exclusively ours.
    let slave = unsafe { OwnedFd::from_raw_fd(slave) };
    set_window_size(slave.as_fd(), size)?;
    set_nonblocking(master.as_fd())?;
    Ok((master, slave))
}

fn open_ptmx() -> io::Result<OwnedFd> {
    let mut attempt = 0;
    loop {
        // SAFETY: posix_openpt returns a fresh descriptor or -1.
        match cvt(unsafe { libc::posix_openpt(libc::O_RDWR | libc::O_NOCTTY) }) {
            // SAFETY: just created and exclusively ours.
            Ok(fd) => return Ok(unsafe { OwnedFd::from_raw_fd(fd) }),
            Err(err) if attempt + 1 < PTMX_ATTEMPTS && is_transient_open_error(&err) => {
                attempt += 1;
            }
            Err(err) => return Err(err),
        }
    }
}

fn is_transient_open_error(err: &io::Error) -> bool {
    matches!(err.raw_os_error(), Some(EREDRIVEOPEN | libc::EINTR))
}

fn set_fd_flag(fd: BorrowedFd<'_>, flag: c_int) -> io::Result<()> {
    // SAFETY: F_GETFD/F_SETFD only read and write descriptor flags of a live descriptor.
    unsafe {
        let flags = cvt(libc::fcntl(fd.as_raw_fd(), libc::F_GETFD))?;
        cvt(libc::fcntl(fd.as_raw_fd(), libc::F_SETFD, flags | flag))?;
    }
    Ok(())
}

fn set_nonblocking(fd: BorrowedFd<'_>) -> io::Result<()> {
    // SAFETY: F_GETFL/F_SETFL only read and write status flags of a live descriptor.
    unsafe {
        let flags = cvt(libc::fcntl(fd.as_raw_fd(), libc::F_GETFL))?;
        cvt(libc::fcntl(
            fd.as_raw_fd(),
            libc::F_SETFL,
            flags | libc::O_NONBLOCK,
        ))?;
    }
    Ok(())
}

/// Tells the line discipline input is UTF-8, so erase works on whole characters.
pub(crate) fn enable_iutf8(fd: BorrowedFd<'_>) -> io::Result<()> {
    let mut termios = MaybeUninit::<libc::termios>::uninit();
    // SAFETY: tcgetattr fully initializes `termios` on success, checked before assume_init.
    let mut termios = unsafe {
        cvt(libc::tcgetattr(fd.as_raw_fd(), termios.as_mut_ptr()))?;
        termios.assume_init()
    };
    termios.c_iflag |= libc::IUTF8;
    // SAFETY: `termios` is a valid, initialized termios for this descriptor.
    cvt(unsafe { libc::tcsetattr(fd.as_raw_fd(), libc::TCSANOW, &raw const termios) })?;
    Ok(())
}

/// Device number of the terminal behind `fd`; equals `e_tdev` of processes it controls.
pub(crate) fn tty_rdev(fd: BorrowedFd<'_>) -> io::Result<u32> {
    let mut stat = MaybeUninit::<libc::stat>::uninit();
    // SAFETY: fstat fully initializes `stat` on success, checked before assume_init.
    let stat = unsafe {
        cvt(libc::fstat(fd.as_raw_fd(), stat.as_mut_ptr()))?;
        stat.assume_init()
    };
    Ok(stat.st_rdev.cast_unsigned())
}

/// Sets the kernel's idea of the terminal size; the foreground group receives `SIGWINCH`.
pub(crate) fn set_window_size(fd: BorrowedFd<'_>, size: libc::winsize) -> io::Result<()> {
    // SAFETY: TIOCSWINSZ reads one `winsize` from a valid pointer.
    cvt(unsafe { libc::ioctl(fd.as_raw_fd(), libc::TIOCSWINSZ, &raw const size) })?;
    Ok(())
}

/// Sends `signal` to `pid`. Callers must guarantee `pid` cannot have been recycled.
pub(crate) fn kill(pid: i32, signal: c_int) -> io::Result<()> {
    // SAFETY: kill has no memory-safety preconditions.
    cvt(unsafe { libc::kill(pid, signal) })?;
    Ok(())
}

/// Installs the post-fork, pre-exec setup that makes `command` a session leader, on its PTY
/// (fd 0) if `controlling_tty`, and otherwise detached from any terminal (the session
/// server).
///
/// `max_fd` bounds the close-on-exec sweep; compute it before forking.
pub(crate) fn install_child_setup(command: &mut Command, max_fd: RawFd, controlling_tty: bool) {
    // SAFETY: the closure only calls async-signal-safe functions (setsid, ioctl, fcntl,
    // signal, sigemptyset, sigprocmask) and allocates nothing, as required between fork and
    // exec in a threaded parent.
    unsafe {
        command.pre_exec(move || child_setup(max_fd, controlling_tty));
    }
}

fn child_setup(max_fd: RawFd, controlling_tty: bool) -> io::Result<()> {
    // SAFETY: async-signal-safe calls on this (child) process only. std has already
    // dup2'ed the PTY slave onto fds 0-2, so fd 0 is the terminal to acquire.
    unsafe {
        cvt(libc::setsid())?;
        if controlling_tty {
            cvt(libc::ioctl(0, libc::TIOCSCTTY.into(), 0))?;
        }
        // Mark (not close) inherited descriptors: std's exec-error pipe must stay usable
        // until exec, and it is already close-on-exec itself (DESIGN.md §4.1 F5).
        for fd in 3..=max_fd {
            let flags = libc::fcntl(fd, libc::F_GETFD);
            if flags >= 0 && flags & libc::FD_CLOEXEC == 0 {
                libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC);
            }
        }
        for signal in RESET_SIGNALS {
            libc::signal(signal, libc::SIG_DFL);
        }
        // The child inherits the forking thread's mask, and GPUI forks on GCD worker threads,
        // which block most signals (F15). A blocked SIGHUP survives every hangup: a crashed or
        // killed kil would leave its jobs running forever.
        let mut empty = MaybeUninit::<libc::sigset_t>::uninit();
        cvt(libc::sigemptyset(empty.as_mut_ptr()))?;
        cvt(libc::sigprocmask(
            libc::SIG_SETMASK,
            empty.as_ptr(),
            ptr::null_mut(),
        ))?;
    }
    Ok(())
}

/// The effective uid of the process at the other end of a Unix socket.
pub(crate) fn peer_uid(socket: BorrowedFd<'_>) -> io::Result<u32> {
    let (mut uid, mut gid) = (0, 0);
    // SAFETY: getpeereid writes two integers through valid pointers.
    cvt(unsafe { libc::getpeereid(socket.as_raw_fd(), &raw mut uid, &raw mut gid) })?;
    Ok(uid)
}

pub(crate) fn effective_uid() -> u32 {
    // SAFETY: geteuid has no preconditions and cannot fail.
    unsafe { libc::geteuid() }
}

/// Takes an exclusive `flock` on `file` without waiting; `false` if another process has it.
pub(crate) fn try_lock(file: BorrowedFd<'_>) -> io::Result<bool> {
    // SAFETY: flock on a live descriptor has no memory-safety preconditions.
    match cvt(unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) }) {
        Ok(_) => Ok(true),
        Err(err) if err.raw_os_error() == Some(libc::EWOULDBLOCK) => Ok(false),
        Err(err) => Err(err),
    }
}

/// Forks; `true` in the child. Only sound while the process has a single thread.
pub(crate) fn fork_single_threaded() -> io::Result<bool> {
    // SAFETY: called first thing in `kil --server`'s main, before any thread exists, so the
    // child is a full copy with no lock held by a vanished thread.
    let pid = cvt(unsafe { libc::fork() })?;
    Ok(pid == 0)
}

/// Registers a one-shot kqueue `NOTE_EXIT` for `child` under `key`.
///
/// Fails with `ESRCH` once the child has begun exiting (it can do so before anyone watches).
/// A failed watch is fully undone, so the same child can be watched again later.
pub(crate) fn watch_exit(poller: &Poller, child: &Child, key: usize) -> io::Result<()> {
    // SAFETY: polling requires the child to stay unreaped while registered. Children are
    // only ever reaped by their owner (`OwnedChild`), after which the one-shot filter has
    // either fired or been deleted by `unwatch_exit`.
    let filter = unsafe { Process::new(child, ProcessOps::Exit) };
    poller
        .add_filter(filter, key, PollMode::Oneshot)
        .inspect_err(|err| {
            // polling records the source before the kernel refuses it; forget it again.
            if err.kind() != io::ErrorKind::AlreadyExists {
                unwatch_exit(poller, child);
            }
        })
}

/// Removes a `watch_exit` registration, including one that already fired: polling keeps a
/// fired one-shot's source, which would refuse the next watch of a recycled PID.
pub(crate) fn unwatch_exit(poller: &Poller, child: &Child) {
    // SAFETY: see `watch_exit`; deleting needs the same (still unreaped) child.
    let filter = unsafe { Process::new(child, ProcessOps::Exit) };
    let _ = poller.delete_filter(filter);
}

/// Arms (or, with `rearm`, re-arms) one-shot timer `id` to fire under `key` after `after`.
pub(crate) fn arm_timer(
    poller: &Poller,
    id: usize,
    after: Duration,
    key: usize,
    rearm: bool,
) -> io::Result<()> {
    let timer = Timer { id, timeout: after };
    if rearm {
        poller.modify_filter(timer, key, PollMode::Oneshot)
    } else {
        poller.add_filter(timer, key, PollMode::Oneshot)
    }
}

/// Removes timer `id`; one that already fired is not an error.
pub(crate) fn disarm_timer(poller: &Poller, id: usize) {
    let _ = poller.delete_filter(Timer {
        id,
        timeout: Duration::ZERO,
    });
}

/// Process facts from `PROC_PIDTBSDINFO`.
#[derive(Clone, Debug)]
pub(crate) struct BsdInfo {
    pub(crate) ppid: i32,
    pub(crate) pgid: i32,
    pub(crate) tdev: u32,
    pub(crate) tpgid: i32,
    pub(crate) start_sec: u64,
    pub(crate) start_usec: u64,
    pub(crate) zombie: bool,
    pub(crate) name: String,
}

fn proc_pidinfo<T>(pid: i32, flavor: c_int) -> Option<T> {
    let mut info = MaybeUninit::<T>::uninit();
    let size = c_int::try_from(size_of::<T>()).ok()?;
    // SAFETY: the kernel writes at most `size` bytes into `info` and returns how many it
    // wrote; only a complete write is accepted before assume_init.
    let written = unsafe { libc::proc_pidinfo(pid, flavor, 0, info.as_mut_ptr().cast(), size) };
    // SAFETY: a full-size write initialized every byte of the plain-data struct.
    (written == size).then(|| unsafe { info.assume_init() })
}

fn c_chars_to_string(chars: &[c_char]) -> String {
    let bytes: Vec<u8> = chars.iter().map(|c| c.cast_unsigned()).collect();
    CStr::from_bytes_until_nul(&bytes)
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Reads `pid`'s BSD info. `None` if it does not exist or belongs to another user.
pub(crate) fn bsd_info(pid: i32) -> Option<BsdInfo> {
    let info: libc::proc_bsdinfo = proc_pidinfo(pid, libc::PROC_PIDTBSDINFO)?;
    let name = match c_chars_to_string(&info.pbi_name) {
        name if name.is_empty() => c_chars_to_string(&info.pbi_comm),
        name => name,
    };
    Some(BsdInfo {
        ppid: info.pbi_ppid.cast_signed(),
        pgid: info.pbi_pgid.cast_signed(),
        tdev: info.e_tdev,
        tpgid: info.e_tpgid.cast_signed(),
        start_sec: info.pbi_start_tvsec,
        start_usec: info.pbi_start_tvusec,
        zombie: info.pbi_status == SZOMB,
        name,
    })
}

/// Current working directory of `pid`.
pub(crate) fn cwd(pid: i32) -> Option<PathBuf> {
    let info: libc::proc_vnodepathinfo = proc_pidinfo(pid, libc::PROC_PIDVNODEPATHINFO)?;
    let path = c_chars_to_string(info.pvi_cdir.vip_path.as_flattened());
    (!path.is_empty()).then(|| PathBuf::from(path))
}

/// Arguments `pid` was started with, `argv[0]` first. `None` for another user's process.
pub(crate) fn proc_args(pid: i32) -> Option<Vec<OsString>> {
    let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid];
    let mut size = 0_usize;
    // SAFETY: a null buffer only asks the kernel for the size, written to `size`.
    let rc = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            3,
            ptr::null_mut(),
            &raw mut size,
            ptr::null_mut(),
            0,
        )
    };
    if rc != 0 || size == 0 {
        return None;
    }
    let mut buf = vec![0_u8; size];
    // SAFETY: the kernel writes at most `size` bytes into `buf` and stores how many it wrote.
    let rc = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            3,
            buf.as_mut_ptr().cast(),
            &raw mut size,
            ptr::null_mut(),
            0,
        )
    };
    if rc != 0 {
        return None;
    }
    buf.truncate(size);
    parse_proc_args(&buf)
}

/// `KERN_PROCARGS2` holds `argc` (native `int`), the executable path, NUL padding, then `argc`
/// NUL-terminated arguments; the environment follows.
fn parse_proc_args(buf: &[u8]) -> Option<Vec<OsString>> {
    let (count, rest) = buf.split_first_chunk::<4>()?;
    let count = usize::try_from(i32::from_ne_bytes(*count)).ok()?;
    let rest = &rest[rest.iter().position(|&b| b == 0)?..];
    let rest = &rest[rest.iter().position(|&b| b != 0)?..];
    let args: Vec<OsString> = rest
        .split(|&b| b == 0)
        .take(count)
        .map(|arg| OsStr::from_bytes(arg).to_os_string())
        .collect();
    (count > 0 && args.len() == count).then_some(args)
}

/// Open descriptor numbers of `pid`.
pub(crate) fn open_fds(pid: i32) -> io::Result<Vec<RawFd>> {
    let mut fds = vec![
        libc::proc_fdinfo {
            proc_fd: 0,
            proc_fdtype: 0
        };
        1024
    ];
    loop {
        let size = c_int::try_from(fds.len() * size_of::<libc::proc_fdinfo>())
            .map_err(io::Error::other)?;
        // SAFETY: the buffer holds `size` bytes of plain-data `proc_fdinfo` slots.
        let written = unsafe {
            libc::proc_pidinfo(pid, libc::PROC_PIDLISTFDS, 0, fds.as_mut_ptr().cast(), size)
        };
        let written = usize::try_from(written).map_err(|_| io::Error::last_os_error())?;
        if written == 0 {
            return Err(io::Error::last_os_error());
        }
        if written < size_of::<libc::proc_fdinfo>() * fds.len() {
            fds.truncate(written / size_of::<libc::proc_fdinfo>());
            return Ok(fds.iter().map(|fd| fd.proc_fd).collect());
        }
        let len = fds.len() * 2;
        fds.resize(
            len,
            libc::proc_fdinfo {
                proc_fd: 0,
                proc_fdtype: 0,
            },
        );
    }
}

/// PIDs whose parent is `pid`, including unreaped zombies.
pub(crate) fn child_pids(pid: i32) -> Vec<i32> {
    let mut pids = vec![0_i32; 4096];
    let Ok(size) = c_int::try_from(pids.len() * size_of::<i32>()) else {
        return Vec::new();
    };
    // SAFETY: the buffer holds `size` bytes; libproc returns the number of PIDs written.
    let count = unsafe { libc::proc_listchildpids(pid, pids.as_mut_ptr().cast(), size) };
    pids.truncate(usize::try_from(count).unwrap_or(0));
    pids.retain(|&pid| pid > 0);
    pids
}

/// Number of threads in `pid`.
pub(crate) fn thread_count(pid: i32) -> Option<u32> {
    let info: libc::proc_taskinfo = proc_pidinfo(pid, libc::PROC_PIDTASKINFO)?;
    u32::try_from(info.pti_threadnum).ok()
}

/// Whether `signal`'s disposition in this process is still `SIG_DFL`.
pub(crate) fn signal_is_default(signal: c_int) -> bool {
    let mut action = MaybeUninit::<libc::sigaction>::zeroed();
    // SAFETY: a null new-action only queries; the kernel fills `action` on success.
    let rc = unsafe { libc::sigaction(signal, ptr::null(), action.as_mut_ptr()) };
    // SAFETY: zero-initialized and, on success, overwritten by the kernel.
    rc == 0 && unsafe { action.assume_init() }.sa_sigaction == libc::SIG_DFL
}

/// Login shell from the password database.
pub(crate) fn passwd_shell() -> Option<PathBuf> {
    let mut buf = vec![0_u8; 4096];
    let mut pwd = MaybeUninit::<libc::passwd>::uninit();
    let mut result: *mut libc::passwd = ptr::null_mut();
    // SAFETY: all pointers are valid for the sizes passed; `result` is null on failure.
    let rc = unsafe {
        libc::getpwuid_r(
            libc::getuid(),
            pwd.as_mut_ptr(),
            buf.as_mut_ptr().cast(),
            buf.len(),
            &raw mut result,
        )
    };
    if rc != 0 || result.is_null() {
        return None;
    }
    // SAFETY: success means `pwd` is initialized and `pw_shell` points into `buf`, which
    // outlives this borrow.
    let shell = unsafe { CStr::from_ptr(pwd.assume_init().pw_shell) };
    let shell = PathBuf::from(OsStr::from_bytes(shell.to_bytes()));
    (!shell.as_os_str().is_empty()).then_some(shell)
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFLocaleCopyCurrent() -> *const c_void;
    fn CFLocaleGetIdentifier(locale: *const c_void) -> *const c_void;
    fn CFStringGetCString(string: *const c_void, buf: *mut c_char, size: isize, enc: u32) -> u8;
    fn CFRelease(object: *const c_void);
}

const CF_STRING_ENCODING_UTF8: u32 = 0x0800_0100;

/// The user's locale identifier from System Settings, e.g. `zh-Hans_CN` or `en_US`.
pub(crate) fn current_locale_identifier() -> Option<String> {
    let mut buf: [c_char; 128] = [0; 128];
    // SAFETY: CFLocaleCopyCurrent returns an owned (+1) locale released below; the
    // identifier follows the Get rule (borrowed from the locale) and is copied out
    // before the release. `buf` is valid for `buf.len()` bytes.
    let ok = unsafe {
        let locale = CFLocaleCopyCurrent();
        if locale.is_null() {
            return None;
        }
        let id = CFLocaleGetIdentifier(locale);
        let ok = !id.is_null()
            && CFStringGetCString(id, buf.as_mut_ptr(), 128, CF_STRING_ENCODING_UTF8) != 0;
        CFRelease(locale);
        ok
    };
    ok.then(|| c_chars_to_string(&buf))
}
