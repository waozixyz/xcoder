//! Build and execute cells inside private Linux namespaces. Neither compilation
//! nor execution inherits the host filesystem, network, environment or desktop.
use std::io::Write;
use std::os::fd::{AsRawFd, FromRawFd};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use tokio::process::{Child, Command};

pub(crate) fn toolchain() -> Result<PathBuf, String> {
    let path = std::env::var_os("XCODER_ZIRAN_TOOLCHAIN")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("XCODER_ZIRAN_TOOLCHAIN_DEFAULT")));
    let path = path
        .canonicalize()
        .map_err(|error| format!("Ziran toolchain: {error}"))?;
    if !path.join("build/bin/ziran").is_file() {
        return Err("XCODER_ZIRAN_TOOLCHAIN must name a built Ziran toolchain".into());
    }
    Ok(path)
}

fn base(memory: u64) -> Command {
    let mut cmd = Command::new(std::env::var_os("XCODER_BWRAP").unwrap_or_else(|| "bwrap".into()));
    cmd.env_clear().args([
        "--unshare-all",
        "--die-with-parent",
        "--new-session",
        "--cap-drop",
        "ALL",
        "--clearenv",
        "--ro-bind",
        "/usr",
        "/usr",
        "--ro-bind",
        "/bin",
        "/bin",
        "--ro-bind",
        "/lib",
        "/lib",
        "--proc",
        "/proc",
        "--dev",
        "/dev",
        "--tmpfs",
        "/tmp",
        "--dir",
        "/etc",
        "--setenv",
        "PATH",
        "/usr/bin:/bin",
        "--setenv",
        "HOME",
        "/tmp",
        "--setenv",
        "TMPDIR",
        "/tmp",
    ]);
    for path in ["/lib64", "/etc/ld.so.cache", "/etc/alternatives"] {
        if Path::new(path).exists() {
            cmd.args(["--ro-bind", path, path]);
        }
    }
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    // SAFETY: only async-signal-safe setrlimit calls run between fork and exec.
    unsafe {
        cmd.pre_exec(move || {
            for (resource, limit) in [
                (libc::RLIMIT_AS, memory),
                (libc::RLIMIT_CPU, 30),
                (libc::RLIMIT_FSIZE, 64 * 1024 * 1024),
                (libc::RLIMIT_CORE, 0),
                (libc::RLIMIT_NOFILE, 256),
            ] {
                let value = libc::rlimit {
                    rlim_cur: limit,
                    rlim_max: limit,
                };
                if libc::setrlimit(resource, &value) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
            }
            Ok(())
        });
    }
    cmd
}

pub(crate) fn compiler(directory: &Path, toolchain: &Path) -> Command {
    let mut cmd = base(1024 * 1024 * 1024);
    for part in ["build/bin", "std", "include"] {
        cmd.arg("--ro-bind")
            .arg(toolchain.join(part))
            .arg(format!("/toolchain/{part}"));
    }
    cmd.arg("--bind").arg(directory).arg("/cell").args([
        "--chdir",
        "/cell",
        "--",
        "/toolchain/build/bin/ziran",
        "build",
        "--target=c",
        "--exe",
        "--entry",
        "main:main",
        "--root",
        "/cell",
        "--module-path",
        "xcoder=/cell/api",
        "-o",
        "/cell/out",
        "/cell/main.zi",
    ]);
    cmd
}

pub(crate) fn cell(directory: &Path, heap_limit: Option<usize>) -> Result<Child, String> {
    let limit = heap_limit
        .map(|value| value as u64)
        .unwrap_or(256 * 1024 * 1024);
    if limit < 16 * 1024 * 1024 {
        return Err("native cell memory limit must be at least 16 MiB".into());
    }
    let mut cmd = base(limit);
    let executable = directory.join("out/main");
    if !std::fs::symlink_metadata(&executable)
        .map_err(|error| error.to_string())?
        .file_type()
        .is_file()
    {
        return Err("Zi2c did not produce a regular executable".into());
    }
    cell_filter(&mut cmd)?;
    // Native code sees only its executable and one callback socket. The source,
    // compiler, other cells, credentials and owner files are absent.
    cmd.arg("--ro-bind")
        .arg(executable)
        .arg("/cell")
        .arg("--bind")
        .arg(directory.join("rpc"))
        .arg("/rpc")
        .args(["--chdir", "/tmp", "--", "/cell"]);
    cmd.spawn()
        .map_err(|error| format!("start native cell sandbox: {error}"))
}

/// Native code can call libc, so isolate process creation as well as files and
/// network access. Pthreads share the cell's address-space limit; process forks
/// cannot multiply that budget. clone3 falls back to the inspectable clone ABI.
fn cell_filter(command: &mut Command) -> Result<(), String> {
    fn instruction(code: u16, jt: u8, jf: u8, k: u32) -> libc::sock_filter {
        libc::sock_filter { code, jt, jf, k }
    }
    #[cfg(target_arch = "x86_64")]
    let arch = 0xc000003e;
    #[cfg(target_arch = "aarch64")]
    let arch = 0xc00000b7;
    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    compile_error!("Xcoder native cell isolation currently supports Linux x86-64 and AArch64");
    const LD: u16 = 0x20;
    const JEQ: u16 = 0x15;
    const RET: u16 = 0x06;
    const ALLOW: u32 = 0x7fff0000;
    const DENY: u32 = 0x00050000 | libc::EPERM as u32;
    let mut filter = vec![
        instruction(LD, 0, 0, 4),
        instruction(JEQ, 1, 0, arch),
        instruction(RET, 0, 0, 0x80000000),
        instruction(LD, 0, 0, 0),
        // Reject x32 syscall numbers before the normal syscall allow/deny list.
        instruction(0x35, 0, 1, 0x40000000),
        instruction(RET, 0, 0, DENY),
        instruction(JEQ, 0, 1, libc::SYS_clone3 as u32),
        instruction(RET, 0, 0, 0x00050000 | libc::ENOSYS as u32),
        instruction(JEQ, 0, 4, libc::SYS_clone as u32),
        instruction(LD, 0, 0, 16),
        instruction(0x45, 1, 0, libc::CLONE_THREAD as u32),
        instruction(RET, 0, 0, DENY),
        instruction(RET, 0, 0, ALLOW),
    ];
    let mut denied = vec![
        libc::SYS_ptrace,
        libc::SYS_bpf,
        libc::SYS_perf_event_open,
        libc::SYS_mount,
        libc::SYS_umount2,
        libc::SYS_unshare,
        libc::SYS_setns,
        libc::SYS_keyctl,
        libc::SYS_add_key,
        libc::SYS_request_key,
        libc::SYS_open_by_handle_at,
    ];
    #[cfg(target_arch = "x86_64")]
    denied.extend([libc::SYS_fork, libc::SYS_vfork]);
    for syscall in denied {
        filter.push(instruction(JEQ, 0, 1, syscall as u32));
        filter.push(instruction(RET, 0, 0, DENY));
    }
    filter.push(instruction(RET, 0, 0, ALLOW));
    // SAFETY: the name is NUL terminated and the returned descriptor is uniquely
    // owned. sock_filter is the Linux fixed-layout ABI, with no padding bytes.
    let fd = unsafe { libc::memfd_create(c"xcoder-cell-policy".as_ptr(), libc::MFD_CLOEXEC) };
    if fd < 0 {
        return Err(std::io::Error::last_os_error().to_string());
    }
    let mut file = unsafe { std::fs::File::from_raw_fd(fd) };
    let bytes = unsafe {
        std::slice::from_raw_parts(
            filter.as_ptr().cast::<u8>(),
            std::mem::size_of_val(filter.as_slice()),
        )
    };
    file.write_all(bytes).map_err(|error| error.to_string())?;
    // Rewind before bubblewrap consumes the inherited filter descriptor.
    use std::io::{Seek, SeekFrom};
    file.seek(SeekFrom::Start(0))
        .map_err(|error| error.to_string())?;
    command.args(["--seccomp", "3"]);
    // SAFETY: dup2/fcntl are async-signal-safe. The closure owns the original fd
    // until spawn, and fd 3 is inherited only by the private bubblewrap child.
    unsafe {
        command.pre_exec(move || {
            if libc::dup2(file.as_raw_fd(), 3) < 0 || libc::fcntl(3, libc::F_SETFD, 0) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    Ok(())
}
