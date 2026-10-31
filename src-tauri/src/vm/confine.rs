//! Confinement of the QEMU process on the host. The guest is the first boundary; this is the
//! second: whoever escapes the guest through a bug in QEMU's device emulation lands in the QEMU
//! process, which runs as the user. So that process is boxed in before it starts, with what each OS
//! offers without administrator rights:
//!
//! - **Linux**: Landlock (the process may read only QEMU's own files, the libraries, the kernel and
//!   the disks of this VM, and write only the VM's own folder; no program outside QEMU's tree and
//!   the library folders can be run), resource limits (no core dumps, a cap on file size and open
//!   files) and, when the bundled QEMU was built with it, QEMU's own seccomp filter (`-sandbox`:
//!   no `fork`/`exec`, no privilege changes). Each part is skipped, with a log line, when the kernel
//!   or the binary does not have it.
//! - **macOS**: `sandbox-exec` with a profile that denies the home folder (except what the VM
//!   uses) and running other programs. Probed once; if the profile does not work on this system the
//!   VM runs unconfined and the app log says so.
//! - **Windows**: a job object that allows no second process, kills QEMU when the app closes and
//!   blocks the desktop/clipboard/handle tricks.
//!
//! Nothing here changes what the guest sees. The setting `confine` (Settings > General > Virtual
//! machines) turns all of it off, for a computer where it gets in the way.

#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;

/// What one VM's QEMU process may touch on the host. Only Linux reads every field (macOS has no
/// `exec`, Windows uses none), so the others would warn about dead code.
#[derive(Debug, Default, Clone)]
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub struct Plan {
    /// Disks it opens: each is readable together with its whole backing chain.
    pub disks: Vec<PathBuf>,
    /// Files and folders it only reads (the kernel, firmware).
    pub read: Vec<PathBuf>,
    /// Folders it may write: the VM's own folder (overlay, sockets).
    pub write: Vec<PathBuf>,
    /// More places programs may be run from (QEMU's own tree and the library folders are always
    /// allowed). Used by tests.
    pub exec: Vec<PathBuf>,
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
/// `disk` and the files it is backed by, found in the qcow2 headers, nearest first. Stops at a
/// file that is not qcow2, has no backing file or names one through a protocol (`json:`).
pub fn backing_chain(disk: &Path) -> Vec<PathBuf> {
    use std::io::{Read, Seek, SeekFrom};
    let mut chain = vec![disk.to_path_buf()];
    for _ in 0..16 {
        let cur = chain.last().unwrap().clone();
        let Ok(mut f) = std::fs::File::open(&cur) else { break };
        let mut head = [0u8; 20];
        if f.read_exact(&mut head).is_err() || head[..4] != [b'Q', b'F', b'I', 0xfb] {
            break;
        }
        let offset = u64::from_be_bytes(head[8..16].try_into().unwrap());
        let size = u32::from_be_bytes(head[16..20].try_into().unwrap()) as usize;
        if offset == 0 || size == 0 || size > 4096 {
            break;
        }
        let mut name = vec![0u8; size];
        if f.seek(SeekFrom::Start(offset)).is_err() || f.read_exact(&mut name).is_err() {
            break;
        }
        let Ok(name) = String::from_utf8(name) else { break };
        if name.starts_with("json:") {
            break;
        }
        let name = name.strip_prefix("file:").unwrap_or(&name);
        let next = Path::new(name);
        let next = if next.is_absolute() { next.to_path_buf() } else { cur.parent().unwrap_or(Path::new(".")).join(next) };
        if chain.contains(&next) {
            break;
        }
        chain.push(next);
    }
    chain
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
/// The bundled QEMU tree that holds `bin/<program>` (`bin/`, `lib/`, `share/qemu/`). A QEMU from
/// the system (`/usr/bin/...`) has no such tree and gets none: only its own file is run from.
fn qemu_root(program: &Path) -> Option<PathBuf> {
    let bin = program.parent()?;
    let root = bin.parent()?;
    (bin.file_name()? == "bin" && root.join("share/qemu").is_dir()).then(|| root.to_path_buf())
}

/// Confines the VM process that `cmd` will start. Call it before spawning (and before `cmd` is
/// turned into a tokio command); on Windows also call `after_spawn` with the child. Never fails:
/// what cannot be applied is logged and skipped.
pub fn apply(cmd: &mut Command, plan: &Plan) {
    #[cfg(target_os = "linux")]
    linux::apply(cmd, plan);
    #[cfg(target_os = "macos")]
    macos::apply(cmd, plan);
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    let _ = (cmd, plan);
}

/// Windows: puts the started VM process into a restricted job. Elsewhere: nothing.
#[cfg_attr(not(windows), allow(unused_variables))]
pub fn after_spawn(child: &tokio::process::Child) {
    #[cfg(windows)]
    windows::after_spawn(child);
}

#[cfg(target_os = "linux")]
mod linux {
    use super::*;
    use std::collections::HashMap;
    use std::ffi::CString;
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::process::CommandExt;
    use std::sync::{Mutex, OnceLock};

    const SYS_CREATE_RULESET: libc::c_long = 444;
    const SYS_ADD_RULE: libc::c_long = 445;
    const SYS_RESTRICT_SELF: libc::c_long = 446;
    const CREATE_RULESET_VERSION: u32 = 1;
    const RULE_PATH_BENEATH: libc::c_int = 1;

    const EXECUTE: u64 = 1 << 0;
    const WRITE_FILE: u64 = 1 << 1;
    const READ_FILE: u64 = 1 << 2;
    const READ_DIR: u64 = 1 << 3;
    const REMOVE_DIR: u64 = 1 << 4;
    const REMOVE_FILE: u64 = 1 << 5;
    const MAKE_CHAR: u64 = 1 << 6;
    const MAKE_DIR: u64 = 1 << 7;
    const MAKE_REG: u64 = 1 << 8;
    const MAKE_SOCK: u64 = 1 << 9;
    const MAKE_FIFO: u64 = 1 << 10;
    const MAKE_BLOCK: u64 = 1 << 11;
    const MAKE_SYM: u64 = 1 << 12;
    const TRUNCATE: u64 = 1 << 14;

    /// Rights a rule on a plain file may carry (the others are for folders).
    const FILE_RIGHTS: u64 = EXECUTE | WRITE_FILE | READ_FILE | TRUNCATE;
    const READ: u64 = READ_FILE | READ_DIR;
    const WRITE: u64 = WRITE_FILE | TRUNCATE | REMOVE_DIR | REMOVE_FILE | MAKE_CHAR | MAKE_DIR | MAKE_REG | MAKE_SOCK | MAKE_FIFO | MAKE_BLOCK | MAKE_SYM;

    #[repr(C, packed)]
    struct PathBeneath {
        allowed: u64,
        parent_fd: i32,
    }

    /// Where programs may be run from and libraries loaded from besides QEMU's tree: the dynamic
    /// loader and libc live here. Not the folders with shells and tools.
    const LIB_DIRS: [&str; 7] = ["/lib", "/lib64", "/usr/lib", "/usr/lib64", "/usr/lib32", "/nix/store", "/gnu/store"];
    /// Read only: name resolution and time zones (`/etc`), CPU and KVM facts (`/proc`, `/sys`),
    /// `resolv.conf` behind its symlink (`/run`), data files of libraries (`/usr/share`).
    const READ_DIRS: [&str; 5] = ["/etc", "/proc", "/sys", "/run", "/usr/share"];
    /// Device nodes a VM process opens.
    const DEVICES: [&str; 6] = ["/dev/kvm", "/dev/null", "/dev/zero", "/dev/full", "/dev/urandom", "/dev/random"];

    fn abi() -> Option<u32> {
        // SAFETY: a plain query; no pointers are passed.
        let v = unsafe { libc::syscall(SYS_CREATE_RULESET, std::ptr::null::<u8>(), 0usize, CREATE_RULESET_VERSION) };
        (v > 0).then_some(v as u32)
    }

    #[cfg(test)]
    pub fn abi_for_tests() -> Option<u32> {
        abi()
    }

    struct Ruleset {
        fd: OwnedFd,
        handled: u64,
    }

    impl Ruleset {
        fn new(abi: u32) -> Result<Ruleset, std::io::Error> {
            let mut handled = EXECUTE | WRITE_FILE | READ_FILE | READ_DIR | REMOVE_DIR | REMOVE_FILE | MAKE_CHAR | MAKE_DIR | MAKE_REG | MAKE_SOCK | MAKE_FIFO | MAKE_BLOCK | MAKE_SYM;
            if abi >= 3 {
                handled |= TRUNCATE;
            }
            // The first field of the attribute is all the oldest kernels know, and all that is needed.
            let attr = handled;
            // SAFETY: `attr` is a valid 8-byte attribute for the duration of the call.
            let fd = unsafe { libc::syscall(SYS_CREATE_RULESET, &attr as *const u64, 8usize, 0u32) };
            if fd < 0 {
                return Err(std::io::Error::last_os_error());
            }
            // SAFETY: the kernel returned a new descriptor that nothing else owns.
            Ok(Ruleset { fd: unsafe { OwnedFd::from_raw_fd(fd as i32) }, handled })
        }

        /// Allows `rights` below `path`. A path that does not exist is skipped.
        fn allow(&self, path: &Path, rights: u64) {
            let Ok(c) = CString::new(path.as_os_str().as_bytes()) else { return };
            // SAFETY: `c` is a valid C string.
            let raw = unsafe { libc::open(c.as_ptr(), libc::O_PATH | libc::O_CLOEXEC) };
            if raw < 0 {
                return;
            }
            // SAFETY: just opened, owned here and closed on drop.
            let parent = unsafe { OwnedFd::from_raw_fd(raw) };
            let is_dir = std::fs::metadata(path).map(|m| m.is_dir()).unwrap_or(false);
            let mut allowed = rights & self.handled;
            if !is_dir {
                allowed &= FILE_RIGHTS;
            }
            let rule = PathBeneath { allowed, parent_fd: parent.as_raw_fd() };
            // SAFETY: `rule` is a valid, live rule attribute and the ruleset fd is open.
            let r = unsafe { libc::syscall(SYS_ADD_RULE, self.fd.as_raw_fd(), RULE_PATH_BENEATH, &rule as *const PathBeneath, 0u32) };
            if r != 0 {
                log::warn!("confine: cannot allow {}: {}", path.display(), std::io::Error::last_os_error());
            }
        }
    }

    /// Whether this QEMU binary has `-sandbox` (the seccomp filter) compiled in. Asked once per binary.
    fn sandbox_supported(program: &Path) -> bool {
        static KNOWN: OnceLock<Mutex<HashMap<PathBuf, bool>>> = OnceLock::new();
        let known = KNOWN.get_or_init(Default::default);
        if let Some(v) = known.lock().unwrap().get(program) {
            return *v;
        }
        // The option is checked where it is read, before `-version` prints and exits.
        let ok = Command::new(program).args(["-sandbox", "on", "-version"]).output().is_ok_and(|o| o.status.success());
        known.lock().unwrap().insert(program.to_path_buf(), ok);
        ok
    }

    pub fn apply(cmd: &mut Command, plan: &Plan) {
        let program = PathBuf::from(cmd.get_program());
        // QEMU's own filter: no new processes, no privilege changes.
        if sandbox_supported(&program) {
            cmd.args(["-sandbox", "on,obsolete=deny,elevateprivileges=deny,spawn=deny,resourcecontrol=deny"]);
        } else {
            log::warn!("confine: this QEMU has no seccomp sandbox (-sandbox); only Landlock and the limits apply");
        }

        let ruleset = match abi() {
            None => {
                log::warn!("confine: this kernel has no Landlock; the QEMU process can read what the user can");
                None
            }
            Some(abi) => match Ruleset::new(abi) {
                Ok(rs) => {
                    let mut run_from: Vec<PathBuf> = LIB_DIRS.iter().map(PathBuf::from).collect();
                    run_from.extend(qemu_root(&program));
                    run_from.push(program.clone());
                    run_from.extend(plan.exec.iter().cloned());
                    for d in &run_from {
                        rs.allow(d, READ | EXECUTE);
                    }
                    for d in READ_DIRS {
                        rs.allow(Path::new(d), READ);
                    }
                    for d in DEVICES {
                        rs.allow(Path::new(d), READ_FILE | WRITE_FILE);
                    }
                    for p in &plan.read {
                        rs.allow(p, READ);
                    }
                    for disk in &plan.disks {
                        for f in backing_chain(disk) {
                            rs.allow(&f, READ_FILE);
                        }
                    }
                    for p in &plan.write {
                        rs.allow(p, READ | WRITE);
                    }
                    Some(rs)
                }
                Err(e) => {
                    log::warn!("confine: Landlock is not usable: {e}");
                    None
                }
            },
        };

        let fd = ruleset.as_ref().map(|r| r.fd.as_raw_fd());
        // After `fork`, before `exec`: only system calls (no allocation, no locks).
        // SAFETY: the closure calls only async-signal-safe functions; `ruleset` is moved into it, so
        // the descriptor stays open until the command is dropped.
        unsafe {
            cmd.pre_exec(move || {
                let _keep = &ruleset;
                let lim = |what, v: u64| {
                    let l = libc::rlimit { rlim_cur: v as libc::rlim_t, rlim_max: v as libc::rlim_t };
                    libc::setrlimit(what, &l);
                };
                // a crashed VM must not write the guest's memory to disk
                lim(libc::RLIMIT_CORE, 0);
                // no runaway file (a disk image is at most some GB); signal on excess
                lim(libc::RLIMIT_FSIZE, 64 << 30);
                if let Some(fd) = fd {
                    if libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                    if libc::syscall(SYS_RESTRICT_SELF, fd, 0u32) != 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                }
                Ok(())
            });
        }
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use super::*;
    use std::sync::OnceLock;

    /// `(subpath "...")` with the characters that matter in the profile's strings escaped.
    fn sub(p: &Path) -> String {
        let s = p.to_string_lossy().replace('\\', "\\\\").replace('"', "\\\"");
        format!("(subpath \"{s}\")")
    }

    /// The profile: everything the system needs stays allowed, then the home folder and running
    /// programs are denied, then what this VM uses is allowed again (the last matching rule wins).
    fn profile(program: &Path, plan: &Plan) -> String {
        let mut s = String::from("(version 1)\n(allow default)\n(deny process-fork)\n(deny process-exec*)\n");
        s.push_str(&format!("(allow process-exec* (literal \"{}\"))\n", program.to_string_lossy().replace('"', "")));
        if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
            s.push_str(&format!("(deny file-read* file-write* {})\n", sub(&home)));
            let mut read: Vec<PathBuf> = plan.read.clone();
            read.extend(qemu_root(program));
            for d in &plan.disks {
                read.extend(backing_chain(d));
            }
            for p in read {
                s.push_str(&format!("(allow file-read* {})\n", sub(&p)));
            }
            for p in &plan.write {
                s.push_str(&format!("(allow file-read* file-write* {})\n", sub(p)));
            }
        }
        s
    }

    pub fn apply(cmd: &mut Command, plan: &Plan) {
        static WORKS: OnceLock<bool> = OnceLock::new();
        let program = PathBuf::from(cmd.get_program());
        let profile = profile(&program, plan);
        // Does QEMU start under such a profile on this system? Asked once.
        let works = *WORKS.get_or_init(|| {
            let ok = Command::new("/usr/bin/sandbox-exec")
                .args(["-p", &profile])
                .arg(&program)
                .arg("--version")
                .output()
                .is_ok_and(|o| o.status.success());
            if !ok {
                log::warn!("confine: sandbox-exec does not run QEMU here; VMs run unconfined");
            }
            ok
        });
        if !works {
            return;
        }
        let mut wrapped = Command::new("/usr/bin/sandbox-exec");
        wrapped.arg("-p").arg(&profile).arg(&program).args(cmd.get_args());
        for (k, v) in cmd.get_envs() {
            match v {
                Some(v) => wrapped.env(k, v),
                None => wrapped.env_remove(k),
            };
        }
        if let Some(dir) = cmd.get_current_dir() {
            wrapped.current_dir(dir);
        }
        *cmd = wrapped;
    }
}

#[cfg(windows)]
mod windows {
    use std::sync::Mutex;

    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::System::JobObjects::*;

    /// Job handles of VMs started so far (as integers, they are only closed here).
    static JOBS: Mutex<Vec<isize>> = Mutex::new(Vec::new());

    pub fn after_spawn(child: &tokio::process::Child) {
        let Some(process) = child.raw_handle() else { return };
        // SAFETY: plain Win32 calls with valid, locally owned structures and handles.
        unsafe {
            let mut jobs = JOBS.lock().unwrap();
            // close the jobs whose VM is gone
            jobs.retain(|&j| {
                let mut info: JOBOBJECT_BASIC_ACCOUNTING_INFORMATION = std::mem::zeroed();
                let ok = QueryInformationJobObject(
                    j as HANDLE,
                    JobObjectBasicAccountingInformation,
                    &mut info as *mut _ as *mut _,
                    std::mem::size_of_val(&info) as u32,
                    std::ptr::null_mut(),
                );
                let alive = ok != 0 && info.ActiveProcesses > 0;
                if !alive {
                    CloseHandle(j as HANDLE);
                }
                alive
            });

            let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            if job.is_null() {
                log::warn!("confine: cannot create a job object: {}", std::io::Error::last_os_error());
                return;
            }
            // one process (QEMU itself, no children), and it dies with the app
            let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE | JOB_OBJECT_LIMIT_ACTIVE_PROCESS;
            limits.BasicLimitInformation.ActiveProcessLimit = 1;
            let ok_limits = SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                &limits as *const _ as *const _,
                std::mem::size_of_val(&limits) as u32,
            );
            let ui = JOBOBJECT_BASIC_UI_RESTRICTIONS {
                UIRestrictionsClass: JOB_OBJECT_UILIMIT_DESKTOP
                    | JOB_OBJECT_UILIMIT_DISPLAYSETTINGS
                    | JOB_OBJECT_UILIMIT_EXITWINDOWS
                    | JOB_OBJECT_UILIMIT_GLOBALATOMS
                    | JOB_OBJECT_UILIMIT_HANDLES
                    | JOB_OBJECT_UILIMIT_READCLIPBOARD
                    | JOB_OBJECT_UILIMIT_SYSTEMPARAMETERS
                    | JOB_OBJECT_UILIMIT_WRITECLIPBOARD,
            };
            SetInformationJobObject(job, JobObjectBasicUIRestrictions, &ui as *const _ as *const _, std::mem::size_of_val(&ui) as u32);
            if ok_limits == 0 || AssignProcessToJobObject(job, process as HANDLE) == 0 {
                log::warn!("confine: cannot put the VM in a job object: {}", std::io::Error::last_os_error());
                CloseHandle(job);
                return;
            }
            jobs.push(job as isize);
        }
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    fn qemu_img() -> Option<PathBuf> {
        let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("resources/qemu/bin/qemu-img");
        p.exists().then_some(p)
    }

    #[test]
    fn qemu_root_is_the_bundled_tree_only() {
        let dir = std::env::temp_dir().join(format!("ccaf-root-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("share/qemu")).unwrap();
        assert_eq!(qemu_root(&dir.join("bin/qemu-system-x86_64")), Some(dir.clone()));
        let _ = std::fs::remove_dir_all(&dir);
        // a system QEMU has no tree of its own: /usr must not become "QEMU's tree"
        assert_eq!(qemu_root(Path::new("/usr/bin/qemu-system-x86_64")), None);
        assert_eq!(qemu_root(Path::new("qemu-system-x86_64")), None);
    }

    #[test]
    fn follows_the_backing_chain() {
        let Some(img) = qemu_img() else { return };
        let dir = std::env::temp_dir().join(format!("ccaf-chain-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        let run = |args: &[&str]| assert!(Command::new(&img).args(args).status().unwrap().success());
        let (a, b, c) = (dir.join("a.qcow2"), dir.join("sub/b.qcow2"), dir.join("c.qcow2"));
        run(&["create", "-q", "-f", "qcow2", a.to_str().unwrap(), "1M"]);
        run(&["create", "-q", "-f", "qcow2", "-b", a.to_str().unwrap(), "-F", "qcow2", b.to_str().unwrap()]);
        // a relative backing name is relative to the overlay's folder
        run(&["create", "-q", "-f", "qcow2", "-b", "sub/b.qcow2", "-F", "qcow2", c.to_str().unwrap()]);
        assert_eq!(backing_chain(&c), vec![c.clone(), dir.join("sub/b.qcow2"), a.clone()]);
        assert_eq!(backing_chain(&a), vec![a]);
        assert_eq!(backing_chain(&dir.join("missing")), vec![dir.join("missing")]);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Landlock for real: a child may read and write what the plan allows and nothing else.
    #[test]
    fn landlock_confines_a_child() {
        if linux::abi_for_tests().is_none() {
            eprintln!("no Landlock on this kernel, skipping");
            return;
        }
        let dir = std::env::temp_dir().join(format!("ccaf-ll-{}", std::process::id()));
        let (ro, rw, other) = (dir.join("ro"), dir.join("rw"), dir.join("other"));
        for d in [&ro, &rw, &other] {
            std::fs::create_dir_all(d).unwrap();
        }
        std::fs::write(ro.join("f"), "r").unwrap();
        std::fs::write(other.join("f"), "secret").unwrap();
        let script = format!(
            "cat {ro}/f >/dev/null || exit 10; echo x >{rw}/n || exit 11; ! cat {other}/f 2>/dev/null || exit 12; ! echo x >{ro}/w 2>/dev/null || exit 13; ! ls /home >/dev/null 2>&1 || exit 14; true",
            ro = ro.display(),
            rw = rw.display(),
            other = other.display()
        );
        let mut cmd = Command::new("/bin/sh");
        cmd.arg("-c").arg(script);
        let plan = Plan { read: vec![ro.clone()], write: vec![rw.clone()], exec: vec!["/bin".into(), "/usr/bin".into()], ..Default::default() };
        apply(&mut cmd, &plan);
        let status = cmd.status().unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        assert!(status.success(), "the confined child misbehaved: {status:?}");
    }

    /// A program outside the allowed folders cannot be run, even when it is executable.
    #[test]
    fn landlock_blocks_other_programs() {
        if linux::abi_for_tests().is_none() {
            return;
        }
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("ccaf-ll-exec-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let prog = dir.join("prog");
        std::fs::write(&prog, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&prog, std::fs::Permissions::from_mode(0o755)).unwrap();
        // the same program runs fine unconfined
        assert!(Command::new(&prog).status().unwrap().success());
        let mut cmd = Command::new("/bin/sh");
        cmd.arg("-c").arg(format!("{} 2>/dev/null; test $? -eq 126", prog.display()));
        let plan = Plan { read: vec![dir.clone()], exec: vec!["/bin".into(), "/usr/bin".into()], ..Default::default() };
        apply(&mut cmd, &plan);
        let out = cmd.status().unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        assert!(out.success(), "{out:?}");
    }
}
