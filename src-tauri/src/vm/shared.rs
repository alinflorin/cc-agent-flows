//! The agents' shared folder (`/home/agent/shared` in the VM): a 9P2000.L server in the app.
//!
//! Each agent VM of a session has a virtio-serial port `shared`; the guest carries it to the
//! kernel's 9p client (see `ccaf-init` and `agentd`), and one `serve` task per VM answers on the
//! other end, all on the same folder (`sessions/<sid>.shared/`). QEMU only moves bytes: nothing
//! of the host's file system is shared by QEMU, and the server only ever acts inside its root.
//!
//! What it guarantees, on every host (the folder must be usable on Linux, macOS and Windows):
//! - paths are lists of names below the root, never strings the guest can bend: no `..` above
//!   the root, no separators, and no symlinks are followed (none can be made);
//! - names that some host cannot hold are refused (`<>:"\|?*`, control characters, Windows device
//!   names, a trailing dot or space), and so is a name that differs from an existing one only in
//!   case;
//! - no symlinks, hard links, device nodes or extended attributes, and every file belongs to the
//!   agent user (`aname` of the attach is `uid:gid`); the executable bit is kept where the host
//!   has one (Unix) and is lost on Windows;
//! - the folder as a whole may not grow past a quota that all the VMs of the session share.
//!
//! Requests are answered one at a time (9P allows that). Not covered: locks (every lock
//! succeeds, the agents do not rely on them), atime, ownership changes.

use std::collections::HashMap;
use std::fs::{File, Metadata, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};

use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::vm::builder::Stream;

// Linux errno values the guest's client understands.
const EPERM: u32 = 1;
const ENOENT: u32 = 2;
const EIO: u32 = 5;
const EACCES: u32 = 13;
const EEXIST: u32 = 17;
const ENOTDIR: u32 = 20;
const EISDIR: u32 = 21;
const EINVAL: u32 = 22;
const EFBIG: u32 = 27;
const ENOSPC: u32 = 28;
const ENAMETOOLONG: u32 = 36;
const ENOTEMPTY: u32 = 39;
const ENOTSUP: u32 = 95;
const EBADF: u32 = 9;

// Message types (9P2000.L).
const TSTATFS: u8 = 8;
const TLOPEN: u8 = 12;
const TLCREATE: u8 = 14;
const TSYMLINK: u8 = 16;
const TMKNOD: u8 = 18;
const TRENAME: u8 = 20;
const TREADLINK: u8 = 22;
const TGETATTR: u8 = 24;
const TSETATTR: u8 = 26;
const TXATTRWALK: u8 = 30;
const TXATTRCREATE: u8 = 32;
const TREADDIR: u8 = 40;
const TFSYNC: u8 = 50;
const TLOCK: u8 = 52;
const TGETLOCK: u8 = 54;
const TLINK: u8 = 70;
const TMKDIR: u8 = 72;
const TRENAMEAT: u8 = 74;
const TUNLINKAT: u8 = 76;
const TVERSION: u8 = 100;
const TAUTH: u8 = 102;
const TATTACH: u8 = 104;
const TFLUSH: u8 = 108;
const TWALK: u8 = 110;
const TREAD: u8 = 116;
const TWRITE: u8 = 118;
const TCLUNK: u8 = 120;
const TREMOVE: u8 = 122;
const RLERROR: u8 = 7;

/// The largest message either side may send.
const MAX_MSIZE: u32 = 1 << 20;
/// The message size the server offers in `Tversion`, whatever the client asks (agentd asks for
/// 256 KiB). In the guest the traffic passes through fifos of 64 KiB (see `agentd`): the kernel's
/// 9p client writes a whole message to one, and a message that does not fit never arrives (a
/// write or read of 64 KiB or more hung the guest for good, measured). Larger messages gain
/// nothing here: with 64 KiB a debug build moves ~300 MB/s.
const OFFER_MSIZE: u32 = 1 << 16;
const AT_REMOVEDIR: u32 = 0x200;
const SETATTR_MODE: u32 = 1;
const SETATTR_SIZE: u32 = 8;
const SETATTR_MTIME: u32 = 0x20;
const SETATTR_MTIME_SET: u32 = 0x100;

/// How many bytes the shared folder of a session may hold, counted across all the VMs that serve it.
pub struct Quota {
    limit: u64,
    used: AtomicU64,
}

impl Quota {
    pub fn reserve(&self, extra: u64) -> Result<(), u32> {
        if extra == 0 {
            return Ok(());
        }
        let mut cur = self.used.load(Ordering::Relaxed);
        loop {
            if cur.saturating_add(extra) > self.limit {
                return Err(ENOSPC);
            }
            match self.used.compare_exchange_weak(cur, cur + extra, Ordering::Relaxed, Ordering::Relaxed) {
                Ok(_) => return Ok(()),
                Err(now) => cur = now,
            }
        }
    }

    pub fn release(&self, bytes: u64) {
        let _ = self.used.try_update(Ordering::Relaxed, Ordering::Relaxed, |u| Some(u.saturating_sub(bytes)));
    }

    pub fn used(&self) -> u64 {
        self.used.load(Ordering::Relaxed)
    }
}

fn dir_size(dir: &Path) -> u64 {
    let mut total = 0;
    for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let Ok(m) = e.path().symlink_metadata() else { continue };
        if m.is_dir() {
            total += dir_size(&e.path());
        } else if m.is_file() {
            total += m.len();
        }
    }
    total
}

/// The quota of the folder `dir`: the VMs of a session that run together share one, so the count
/// is not each server's own. Made (and the folder measured) when no server has it.
pub fn quota_for(dir: &Path, limit: u64) -> Arc<Quota> {
    static ALL: Mutex<Option<HashMap<PathBuf, Weak<Quota>>>> = Mutex::new(None);
    let mut all = ALL.lock().unwrap();
    let map = all.get_or_insert_with(HashMap::new);
    map.retain(|_, w| w.strong_count() > 0);
    if let Some(q) = map.get(dir).and_then(Weak::upgrade) {
        return q;
    }
    let q = Arc::new(Quota { limit, used: AtomicU64::new(dir_size(dir)) });
    map.insert(dir.to_path_buf(), Arc::downgrade(&q));
    q
}

/// Whether a host could hold a file of this name; the guest sees `EINVAL` otherwise.
pub fn check_name(n: &str) -> Result<(), u32> {
    if n.is_empty() || n == "." || n == ".." {
        return Err(EINVAL);
    }
    if n.len() > 255 {
        return Err(ENAMETOOLONG);
    }
    if n.chars().any(|c| c.is_control() || "<>:\"/\\|?*".contains(c)) || n.ends_with('.') || n.ends_with(' ') {
        return Err(EINVAL);
    }
    let stem = n.split('.').next().unwrap_or("").trim_end().to_ascii_uppercase();
    let reserved = ["CON", "PRN", "AUX", "NUL"].contains(&stem.as_str())
        || ((stem.starts_with("COM") || stem.starts_with("LPT")) && stem.len() == 4 && stem.as_bytes()[3].is_ascii_digit() && stem.as_bytes()[3] != b'0');
    if reserved { Err(EINVAL) } else { Ok(()) }
}

fn errno(e: &io::Error) -> u32 {
    use io::ErrorKind::*;
    match e.kind() {
        NotFound => ENOENT,
        PermissionDenied => EACCES,
        AlreadyExists => EEXIST,
        DirectoryNotEmpty => ENOTEMPTY,
        NotADirectory => ENOTDIR,
        IsADirectory => EISDIR,
        InvalidInput | InvalidData => EINVAL,
        StorageFull => ENOSPC,
        FileTooLarge => EFBIG,
        _ => EIO,
    }
}

// ---- wire format

struct Rd<'a> {
    b: &'a [u8],
    o: usize,
}

impl<'a> Rd<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], u32> {
        let end = self.o.checked_add(n).filter(|e| *e <= self.b.len()).ok_or(EINVAL)?;
        let s = &self.b[self.o..end];
        self.o = end;
        Ok(s)
    }
    fn u8(&mut self) -> Result<u8, u32> {
        Ok(self.take(1)?[0])
    }
    fn u16(&mut self) -> Result<u16, u32> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into().unwrap()))
    }
    fn u32(&mut self) -> Result<u32, u32> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn u64(&mut self) -> Result<u64, u32> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }
    fn str(&mut self) -> Result<String, u32> {
        let n = self.u16()? as usize;
        String::from_utf8(self.take(n)?.to_vec()).map_err(|_| EINVAL)
    }
}

#[derive(Default)]
struct Wr(Vec<u8>);

impl Wr {
    fn u8(&mut self, v: u8) -> &mut Self {
        self.0.push(v);
        self
    }
    fn u16(&mut self, v: u16) -> &mut Self {
        self.0.extend_from_slice(&v.to_le_bytes());
        self
    }
    fn u32(&mut self, v: u32) -> &mut Self {
        self.0.extend_from_slice(&v.to_le_bytes());
        self
    }
    fn u64(&mut self, v: u64) -> &mut Self {
        self.0.extend_from_slice(&v.to_le_bytes());
        self
    }
    fn str(&mut self, s: &str) -> &mut Self {
        self.u16(s.len() as u16);
        self.0.extend_from_slice(s.as_bytes());
        self
    }
    fn bytes(&mut self, b: &[u8]) -> &mut Self {
        self.0.extend_from_slice(b);
        self
    }
}

// ---- files

/// Metadata of the entry itself; a symlink is an error, never followed.
fn meta(p: &Path) -> io::Result<Metadata> {
    let m = p.symlink_metadata()?;
    if m.file_type().is_symlink() {
        return Err(io::Error::new(io::ErrorKind::PermissionDenied, "symlink"));
    }
    Ok(m)
}

/// The names in `dir` that equal `name` ignoring case, as stored.
fn same_ignoring_case(dir: &Path, name: &str) -> Vec<String> {
    std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| n.eq_ignore_ascii_case(name))
        .collect()
}

/// `meta`, but on a host whose file system ignores case the name must match exactly.
fn meta_exact(dir: &Path, name: &str) -> io::Result<Metadata> {
    let p = dir.join(name);
    let m = meta(&p)?;
    if cfg!(any(windows, target_os = "macos")) && !same_ignoring_case(dir, name).iter().any(|n| n == name) {
        return Err(io::ErrorKind::NotFound.into());
    }
    Ok(m)
}

/// A new name in `dir`: valid, and not the same as an existing one in other letters.
fn new_name_ok(dir: &Path, name: &str) -> Result<(), u32> {
    check_name(name)?;
    if same_ignoring_case(dir, name).iter().any(|n| n != name) {
        return Err(EEXIST);
    }
    Ok(())
}

#[cfg(unix)]
fn exec_bit(m: &Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    m.permissions().mode() & 0o111 != 0
}
#[cfg(not(unix))]
fn exec_bit(_: &Metadata) -> bool {
    false
}

#[cfg(unix)]
fn set_exec(p: &Path, on: bool) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut perm = std::fs::metadata(p)?.permissions();
    let mode = if on { perm.mode() | 0o111 } else { perm.mode() & !0o111 };
    perm.set_mode(mode);
    std::fs::set_permissions(p, perm)
}
#[cfg(not(unix))]
fn set_exec(_: &Path, _: bool) -> io::Result<()> {
    Ok(())
}

#[cfg(unix)]
fn read_at(f: &File, buf: &mut [u8], off: u64) -> io::Result<usize> {
    std::os::unix::fs::FileExt::read_at(f, buf, off)
}
#[cfg(windows)]
fn read_at(f: &File, buf: &mut [u8], off: u64) -> io::Result<usize> {
    std::os::windows::fs::FileExt::seek_read(f, buf, off)
}
#[cfg(unix)]
fn write_at(f: &File, buf: &[u8], off: u64) -> io::Result<usize> {
    std::os::unix::fs::FileExt::write_at(f, buf, off)
}
#[cfg(windows)]
fn write_at(f: &File, buf: &[u8], off: u64) -> io::Result<usize> {
    std::os::windows::fs::FileExt::seek_write(f, buf, off)
}

fn secs(t: io::Result<std::time::SystemTime>) -> u64 {
    t.ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map_or(0, |d| d.as_secs())
}

// ---- what the user does with the folder (the session's Files tab)

/// An entry of the folder as the Files tab lists it.
#[derive(serde::Serialize)]
pub struct FileEntry {
    pub name: String,
    pub dir: bool,
    pub size: u64,
    /// Seconds since the Unix epoch.
    pub modified: u64,
}

fn name_error(code: u32) -> String {
    match code {
        EEXIST => "a file with this name in other letters already exists".into(),
        ENAMETOOLONG => "the name is too long (255 bytes at most)".into(),
        _ => "this name cannot be used: it has a character that some system does not allow (<>:\"/\\|?*), a control character, a reserved name (CON, NUL, COM1...) or ends with a dot or space".into(),
    }
}

/// `rel` ("a/b", "" for the root) below `root`: every part must be a plain name and none of the
/// folders on the way may be a symlink, so the answer is always inside `root`.
fn resolve(root: &Path, rel: &str) -> Result<PathBuf, String> {
    let mut p = root.to_path_buf();
    for part in rel.split('/').filter(|s| !s.is_empty()) {
        if part == "." || part == ".." || part.contains(['\\', '\0']) {
            return Err("bad path".into());
        }
        p.push(part);
        if let Ok(m) = p.symlink_metadata()
            && m.file_type().is_symlink()
        {
            return Err("bad path".into());
        }
    }
    Ok(p)
}

pub fn list(root: &Path, rel: &str) -> Result<Vec<FileEntry>, String> {
    let dir = resolve(root, rel)?;
    let Ok(rd) = std::fs::read_dir(&dir) else { return Ok(vec![]) };
    let mut out: Vec<FileEntry> = rd
        .flatten()
        .filter_map(|e| {
            let m = e.path().symlink_metadata().ok().filter(|m| !m.file_type().is_symlink())?;
            Some(FileEntry { name: e.file_name().into_string().ok()?, dir: m.is_dir(), size: if m.is_dir() { 0 } else { m.len() }, modified: secs(m.modified()) })
        })
        .collect();
    out.sort_by(|a, b| b.dir.cmp(&a.dir).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
    Ok(out)
}

pub fn make_dir(root: &Path, rel: &str, name: &str) -> Result<(), String> {
    let dir = resolve(root, rel)?;
    new_name_ok(&dir, name).map_err(name_error)?;
    std::fs::create_dir(dir.join(name)).map_err(|e| e.to_string())
}

/// Deletes a file or a folder with everything in it and gives its bytes back to the quota.
pub fn remove(root: &Path, rel: &str, quota: &Quota) -> Result<(), String> {
    let p = resolve(root, rel)?;
    if p == root {
        return Err("the folder itself cannot be deleted".into());
    }
    let m = p.symlink_metadata().map_err(|e| e.to_string())?;
    if m.is_dir() {
        let bytes = dir_size(&p);
        std::fs::remove_dir_all(&p).map_err(|e| e.to_string())?;
        quota.release(bytes);
    } else {
        std::fs::remove_file(&p).map_err(|e| e.to_string())?;
        quota.release(m.len());
    }
    Ok(())
}

/// Copies the file `src` into the folder `rel`, under its own name, if the name is acceptable, no
/// file of that name is there and the quota allows it.
pub fn import_file(root: &Path, rel: &str, src: &Path, quota: &Quota) -> Result<(), String> {
    let dir = resolve(root, rel)?;
    let name = src.file_name().and_then(|n| n.to_str()).ok_or("the file name is not valid text")?;
    new_name_ok(&dir, name).map_err(name_error)?;
    let target = dir.join(name);
    if target.symlink_metadata().is_ok() {
        return Err(format!("{name} already exists"));
    }
    let meta = std::fs::metadata(src).map_err(|e| e.to_string())?;
    if !meta.is_file() {
        return Err(format!("{name} is not a file"));
    }
    quota.reserve(meta.len()).map_err(|_| format!("{name} does not fit in the folder's quota"))?;
    if let Err(e) = std::fs::copy(src, &target) {
        let _ = std::fs::remove_file(&target);
        quota.release(meta.len());
        return Err(e.to_string());
    }
    Ok(())
}

fn copy_tree(from: &Path, to: &Path) -> Result<(), String> {
    std::fs::create_dir_all(to).map_err(|e| e.to_string())?;
    for e in std::fs::read_dir(from).map_err(|e| e.to_string())?.flatten() {
        let m = e.path().symlink_metadata().map_err(|e| e.to_string())?;
        let target = to.join(e.file_name());
        if m.file_type().is_symlink() {
            continue;
        } else if m.is_dir() {
            copy_tree(&e.path(), &target)?;
        } else {
            std::fs::copy(e.path(), &target).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

/// Copies a file to the path `dest`, or a folder to `dest/<its name>`.
pub fn export(root: &Path, rel: &str, dest: &Path) -> Result<(), String> {
    let p = resolve(root, rel)?;
    let m = p.symlink_metadata().map_err(|e| e.to_string())?;
    if m.is_dir() {
        let name = p.file_name().map_or_else(|| "shared".to_string(), |n| n.to_string_lossy().into_owned());
        copy_tree(&p, &dest.join(name))
    } else {
        std::fs::copy(&p, dest).map(|_| ()).map_err(|e| e.to_string())
    }
}

// ---- the server

struct Fid {
    /// Names below the root; empty is the root itself.
    path: Vec<String>,
    /// The file when the fid was opened (`Tlopen`, `Tlcreate`); folders are read by path.
    file: Option<File>,
}

struct Server {
    root: PathBuf,
    quota: Arc<Quota>,
    fids: HashMap<u32, Fid>,
    msize: u32,
    uid: u32,
    gid: u32,
}

type Res = Result<Vec<u8>, u32>;

impl Server {
    fn real(&self, path: &[String]) -> PathBuf {
        path.iter().fold(self.root.clone(), |p, n| p.join(n))
    }

    fn fid(&self, id: u32) -> Result<&Fid, u32> {
        self.fids.get(&id).ok_or(EBADF)
    }

    fn qid(path: &[String], m: &Metadata) -> [u8; 13] {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        path.hash(&mut h);
        let mut q = [0u8; 13];
        q[0] = if m.is_dir() { 0x80 } else { 0 };
        q[1..5].copy_from_slice(&(secs(m.modified()) as u32).to_le_bytes());
        q[5..13].copy_from_slice(&h.finish().to_le_bytes());
        q
    }

    /// Runs one request and returns the body of its answer.
    fn handle(&mut self, typ: u8, body: &[u8]) -> Res {
        let mut r = Rd { b: body, o: 0 };
        let mut w = Wr::default();
        match typ {
            TVERSION => {
                let m = r.u32()?;
                let v = r.str()?;
                self.msize = m.clamp(4096, OFFER_MSIZE);
                self.fids.clear();
                w.u32(self.msize).str(if v.starts_with("9P2000.L") { "9P2000.L" } else { "unknown" });
            }
            TAUTH => return Err(ENOTSUP),
            TATTACH => {
                let (fid, _afid) = (r.u32()?, r.u32()?);
                let _uname = r.str()?;
                let aname = r.str()?;
                if let Some((u, g)) = aname.split_once(':')
                    && let (Ok(u), Ok(g)) = (u.parse(), g.parse())
                {
                    (self.uid, self.gid) = (u, g);
                }
                let m = meta(&self.root).map_err(|e| errno(&e))?;
                w.bytes(&Self::qid(&[], &m));
                self.fids.insert(fid, Fid { path: vec![], file: None });
            }
            TFLUSH => {}
            TWALK => {
                let fid = r.u32()?;
                let newfid = r.u32()?;
                let n = r.u16()? as usize;
                let mut path = self.fid(fid)?.path.clone();
                let mut qids: Vec<[u8; 13]> = vec![];
                for _ in 0..n {
                    let name = r.str()?;
                    if name == ".." {
                        path.pop();
                    } else if name != "." {
                        if check_name(&name).is_err() {
                            break;
                        }
                        let dir = self.real(&path);
                        if meta_exact(&dir, &name).is_err() {
                            break;
                        }
                        path.push(name);
                    }
                    match meta(&self.real(&path)) {
                        Ok(m) => qids.push(Self::qid(&path, &m)),
                        Err(_) => break,
                    }
                }
                if n > 0 && qids.is_empty() {
                    return Err(ENOENT);
                }
                if qids.len() == n {
                    self.fids.insert(newfid, Fid { path, file: None });
                }
                w.u16(qids.len() as u16);
                for q in qids {
                    w.bytes(&q);
                }
            }
            TGETATTR => {
                let fid = r.u32()?;
                let path = self.fid(fid)?.path.clone();
                let m = meta(&self.real(&path)).map_err(|e| errno(&e))?;
                let mode = if m.is_dir() { 0o040755 } else if exec_bit(&m) { 0o100755 } else { 0o100644 };
                let (mt, ct) = (secs(m.modified()), secs(m.created().or(m.modified())));
                w.u64(0x7ff).bytes(&Self::qid(&path, &m));
                w.u32(mode).u32(self.uid).u32(self.gid).u64(1).u64(0).u64(m.len()).u64(4096).u64(m.len().div_ceil(512));
                w.u64(mt).u64(0).u64(mt).u64(0).u64(ct).u64(0).u64(0).u64(0).u64(0).u64(0);
            }
            TSETATTR => {
                let fid = r.u32()?;
                let valid = r.u32()?;
                let (mode, _uid, _gid, size) = (r.u32()?, r.u32()?, r.u32()?, r.u64()?);
                let (_as, _an, ms, _mn) = (r.u64()?, r.u64()?, r.u64()?, r.u64()?);
                let path = self.fid(fid)?.path.clone();
                let real = self.real(&path);
                let m = meta(&real).map_err(|e| errno(&e))?;
                if valid & SETATTR_SIZE != 0 {
                    if m.is_dir() {
                        return Err(EISDIR);
                    }
                    if size > m.len() {
                        self.quota.reserve(size - m.len())?;
                    }
                    let f = match &self.fids[&fid].file {
                        Some(f) if f.metadata().is_ok() => f.try_clone(),
                        _ => OpenOptions::new().write(true).open(&real),
                    }
                    .map_err(|e| errno(&e))?;
                    if let Err(e) = f.set_len(size) {
                        if size > m.len() {
                            self.quota.release(size - m.len());
                        }
                        return Err(errno(&e));
                    }
                    if size < m.len() {
                        self.quota.release(m.len() - size);
                    }
                }
                if valid & SETATTR_MODE != 0 && !m.is_dir() {
                    set_exec(&real, mode & 0o111 != 0).map_err(|e| errno(&e))?;
                }
                if valid & SETATTR_MTIME != 0 && valid & SETATTR_MTIME_SET != 0 && !m.is_dir() {
                    let t = std::time::UNIX_EPOCH + std::time::Duration::from_secs(ms);
                    if let Ok(f) = OpenOptions::new().write(true).open(&real) {
                        let _ = f.set_modified(t);
                    }
                }
            }
            TLOPEN => {
                let (fid, flags) = (r.u32()?, r.u32()?);
                let path = self.fid(fid)?.path.clone();
                let real = self.real(&path);
                let m = meta(&real).map_err(|e| errno(&e))?;
                let mut file = None;
                if m.is_file() {
                    let acc = flags & 3;
                    let mut o = OpenOptions::new();
                    o.read(acc != 1).write(acc != 0);
                    let f = o.open(&real).map_err(|e| errno(&e))?;
                    if flags & 0o1000 != 0 && acc != 0 {
                        // O_TRUNC
                        f.set_len(0).map_err(|e| errno(&e))?;
                        self.quota.release(m.len());
                    }
                    file = Some(f);
                } else if flags & 3 != 0 {
                    return Err(EISDIR);
                }
                self.fids.get_mut(&fid).unwrap().file = file;
                w.bytes(&Self::qid(&path, &m)).u32(0);
            }
            TLCREATE => {
                let (fid, name) = (r.u32()?, r.str()?);
                let (flags, mode, _gid) = (r.u32()?, r.u32()?, r.u32()?);
                let dirpath = self.fid(fid)?.path.clone();
                let dir = self.real(&dirpath);
                if !meta(&dir).map_err(|e| errno(&e))?.is_dir() {
                    return Err(ENOTDIR);
                }
                new_name_ok(&dir, &name)?;
                let acc = flags & 3;
                let f = OpenOptions::new().read(acc != 1).write(true).create_new(true).open(dir.join(&name)).map_err(|e| errno(&e))?;
                if mode & 0o111 != 0 {
                    let _ = set_exec(&dir.join(&name), true);
                }
                let mut path = dirpath;
                path.push(name);
                let m = meta(&self.real(&path)).map_err(|e| errno(&e))?;
                w.bytes(&Self::qid(&path, &m)).u32(0);
                self.fids.insert(fid, Fid { path, file: Some(f) });
            }
            TMKDIR => {
                let (dfid, name) = (r.u32()?, r.str()?);
                let dirpath = self.fid(dfid)?.path.clone();
                let dir = self.real(&dirpath);
                new_name_ok(&dir, &name)?;
                std::fs::create_dir(dir.join(&name)).map_err(|e| errno(&e))?;
                let mut path = dirpath;
                path.push(name);
                let m = meta(&self.real(&path)).map_err(|e| errno(&e))?;
                w.bytes(&Self::qid(&path, &m));
            }
            TREAD => {
                let (fid, off, count) = (r.u32()?, r.u64()?, r.u32()?);
                let f = self.fid(fid)?.file.as_ref().ok_or(EBADF)?;
                let mut buf = vec![0u8; count.min(self.msize.saturating_sub(24)) as usize];
                let n = read_at(f, &mut buf, off).map_err(|e| errno(&e))?;
                w.u32(n as u32).bytes(&buf[..n]);
            }
            TWRITE => {
                let (fid, off, count) = (r.u32()?, r.u64()?, r.u32()? as usize);
                let data = r.take(count)?;
                let f = self.fid(fid)?.file.as_ref().ok_or(EBADF)?;
                let len = f.metadata().map_err(|e| errno(&e))?.len();
                let end = off.checked_add(count as u64).ok_or(EFBIG)?;
                let grow = end.saturating_sub(len);
                self.quota.reserve(grow)?;
                let mut done = 0;
                while done < data.len() {
                    match write_at(f, &data[done..], off + done as u64) {
                        Ok(0) => break,
                        Ok(n) => done += n,
                        Err(e) => {
                            self.quota.release(grow);
                            return Err(errno(&e));
                        }
                    }
                }
                w.u32(done as u32);
            }
            TREADDIR => {
                let (fid, off, count) = (r.u32()?, r.u64()?, r.u32()?);
                let path = self.fid(fid)?.path.clone();
                let dir = self.real(&path);
                let mut names: Vec<String> = std::fs::read_dir(&dir).map_err(|e| errno(&e))?.flatten().filter_map(|e| e.file_name().into_string().ok()).filter(|n| check_name(n).is_ok()).collect();
                names.sort();
                let mut out = Wr::default();
                let limit = count.min(self.msize.saturating_sub(32)) as usize;
                let all = [".", ".."].map(String::from).into_iter().chain(names);
                for (i, name) in all.enumerate().skip(off as usize) {
                    let mut p = path.clone();
                    if name == ".." {
                        p.pop();
                    } else if name != "." {
                        p.push(name.clone());
                    }
                    let Ok(m) = meta(&self.real(&p)) else { continue };
                    let mut e = Wr::default();
                    e.bytes(&Self::qid(&p, &m)).u64(i as u64 + 1).u8(if m.is_dir() { 4 } else { 8 }).str(&name);
                    if out.0.len() + e.0.len() > limit {
                        break;
                    }
                    out.bytes(&e.0);
                }
                w.u32(out.0.len() as u32).bytes(&out.0);
            }
            TFSYNC => {
                let _ = r.u32()?;
            }
            TSTATFS => {
                let blocks = self.quota.limit / 4096;
                let free = self.quota.limit.saturating_sub(self.quota.used()) / 4096;
                w.u32(0x0102_1997).u32(4096).u64(blocks).u64(free).u64(free).u64(1 << 20).u64(1 << 20).u64(0).u32(255);
            }
            TCLUNK => {
                self.fids.remove(&r.u32()?);
            }
            TREMOVE => {
                let fid = r.u32()?;
                let f = self.fids.remove(&fid).ok_or(EBADF)?;
                let (name, parent) = f.path.split_last().ok_or(EPERM)?;
                self.unlink(parent, name, false)?;
            }
            TUNLINKAT => {
                let (dfid, name, flags) = (r.u32()?, r.str()?, r.u32()?);
                let parent = self.fid(dfid)?.path.clone();
                self.unlink(&parent, &name, flags & AT_REMOVEDIR != 0)?;
            }
            TRENAME => {
                let (fid, dfid, name) = (r.u32()?, r.u32()?, r.str()?);
                let from = self.fid(fid)?.path.clone();
                let to_dir = self.fid(dfid)?.path.clone();
                let new = self.rename(&from, &to_dir, &name)?;
                self.fids.get_mut(&fid).unwrap().path = new;
            }
            TRENAMEAT => {
                let (odir, oname, ndir, nname) = (r.u32()?, r.str()?, r.u32()?, r.str()?);
                let mut from = self.fid(odir)?.path.clone();
                check_name(&oname)?;
                from.push(oname);
                let to_dir = self.fid(ndir)?.path.clone();
                self.rename(&from, &to_dir, &nname)?;
            }
            TLOCK => {
                w.u8(0); // every lock succeeds
            }
            TGETLOCK => {
                let _ = r.u32()?;
                let (_t, start, len, pid) = (r.u8()?, r.u64()?, r.u64()?, r.u32()?);
                w.u8(2).u64(start).u64(len).u32(pid).str("");
            }
            TSYMLINK | TMKNOD | TLINK => return Err(EPERM),
            TREADLINK => return Err(EINVAL),
            TXATTRWALK | TXATTRCREATE => return Err(ENOTSUP),
            _ => return Err(ENOTSUP),
        }
        Ok(w.0)
    }

    fn unlink(&mut self, parent: &[String], name: &str, dir: bool) -> Result<(), u32> {
        check_name(name)?;
        let d = self.real(parent);
        let m = meta_exact(&d, name).map_err(|e| errno(&e))?;
        let p = d.join(name);
        if dir {
            if !m.is_dir() {
                return Err(ENOTDIR);
            }
            std::fs::remove_dir(&p).map_err(|e| errno(&e))
        } else {
            if m.is_dir() {
                return Err(EISDIR);
            }
            std::fs::remove_file(&p).map_err(|e| errno(&e))?;
            self.quota.release(m.len());
            Ok(())
        }
    }

    fn rename(&mut self, from: &[String], to_dir: &[String], name: &str) -> Result<Vec<String>, u32> {
        let (old_name, old_parent) = from.split_last().ok_or(EPERM)?;
        let src_dir = self.real(old_parent);
        meta_exact(&src_dir, old_name).map_err(|e| errno(&e))?;
        let dst_dir = self.real(to_dir);
        if !meta(&dst_dir).map_err(|e| errno(&e))?.is_dir() {
            return Err(ENOTDIR);
        }
        // a rename that only changes case is the one case where the name may differ in letters
        if !old_name.eq_ignore_ascii_case(name) || src_dir != dst_dir {
            new_name_ok(&dst_dir, name)?;
        } else {
            check_name(name)?;
        }
        let target = dst_dir.join(name);
        if let Ok(t) = meta(&target)
            && name != old_name
        {
            // the target is replaced, as rename(2) does; its bytes leave the quota
            if t.is_dir() {
                return Err(EEXIST);
            }
            self.quota.release(t.len());
        }
        std::fs::rename(src_dir.join(old_name), &target).map_err(|e| errno(&e))?;
        let mut path = to_dir.to_vec();
        path.push(name.to_string());
        Ok(path)
    }
}

/// Serves `root` to one VM over `stream` until the VM closes its end.
pub async fn serve(stream: Box<dyn Stream>, root: PathBuf, quota: Arc<Quota>) {
    let root = match std::fs::create_dir_all(&root).and_then(|_| root.canonicalize()) {
        Ok(r) => r,
        Err(e) => {
            log::warn!("shared folder: {}: {e}", root.display());
            return;
        }
    };
    let (mut rd, mut wr) = tokio::io::split(stream);
    let mut s = Server { root, quota, fids: HashMap::new(), msize: 8192, uid: 1000, gid: 1000 };
    loop {
        let mut head = [0u8; 7];
        if rd.read_exact(&mut head).await.is_err() {
            return;
        }
        let size = u32::from_le_bytes(head[0..4].try_into().unwrap());
        let (typ, tag) = (head[4], u16::from_le_bytes(head[5..7].try_into().unwrap()));
        if !(7..=MAX_MSIZE + 64).contains(&size) {
            log::warn!("shared folder: a message of {size} bytes, closing");
            return;
        }
        let mut body = vec![0u8; size as usize - 7];
        if rd.read_exact(&mut body).await.is_err() {
            return;
        }
        let res = tokio::task::block_in_place(|| s.handle(typ, &body));
        let mut out = Wr::default();
        match res {
            Ok(b) => {
                out.u32(7 + b.len() as u32).u8(typ + 1).u16(tag).bytes(&b);
            }
            Err(e) => {
                out.u32(11).u8(RLERROR).u16(tag).u32(e);
            }
        }
        if wr.write_all(&out.0).await.is_err() || wr.flush().await.is_err() {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream};

    struct Client {
        s: DuplexStream,
        tag: u16,
    }

    impl Client {
        async fn call(&mut self, typ: u8, body: &[u8]) -> Result<Vec<u8>, u32> {
            self.tag += 1;
            let mut m = ((7 + body.len()) as u32).to_le_bytes().to_vec();
            m.push(typ);
            m.extend_from_slice(&self.tag.to_le_bytes());
            m.extend_from_slice(body);
            self.s.write_all(&m).await.unwrap();
            let mut head = [0u8; 7];
            self.s.read_exact(&mut head).await.unwrap();
            let size = u32::from_le_bytes(head[0..4].try_into().unwrap()) as usize;
            let mut b = vec![0u8; size - 7];
            self.s.read_exact(&mut b).await.unwrap();
            assert_eq!(u16::from_le_bytes(head[5..7].try_into().unwrap()), self.tag);
            if head[4] == RLERROR {
                return Err(u32::from_le_bytes(b[..4].try_into().unwrap()));
            }
            assert_eq!(head[4], typ + 1);
            Ok(b)
        }
    }

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ccaf-9p-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    async fn start(root: &Path, limit: u64) -> Client {
        let (a, b) = tokio::io::duplex(1 << 20);
        tokio::spawn(serve(Box::new(b), root.to_path_buf(), quota_for(root, limit)));
        let mut c = Client { s: a, tag: 0 };
        let mut v = Wr::default();
        v.u32(65536).str("9P2000.L");
        c.call(TVERSION, &v.0).await.unwrap();
        let mut at = Wr::default();
        at.u32(0).u32(u32::MAX).str("agent").str("1234:5678").u32(0);
        c.call(TATTACH, &at.0).await.unwrap();
        c
    }

    fn walk(fid: u32, newfid: u32, names: &[&str]) -> Vec<u8> {
        let mut w = Wr::default();
        w.u32(fid).u32(newfid).u16(names.len() as u16);
        for n in names {
            w.str(n);
        }
        w.0
    }

    fn create(fid: u32, name: &str) -> Vec<u8> {
        let mut w = Wr::default();
        w.u32(fid).str(name).u32(2).u32(0o644).u32(0);
        w.0
    }

    fn write(fid: u32, off: u64, data: &[u8]) -> Vec<u8> {
        let mut w = Wr::default();
        w.u32(fid).u64(off).u32(data.len() as u32).bytes(data);
        w.0
    }

    fn read(fid: u32, off: u64, n: u32) -> Vec<u8> {
        let mut w = Wr::default();
        w.u32(fid).u64(off).u32(n);
        w.0
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn files_round_trip_and_belong_to_the_agent() {
        let root = tmp("rt");
        let mut c = start(&root, 1 << 20).await;
        c.call(TWALK, &walk(0, 1, &[])).await.unwrap();
        c.call(TLCREATE, &create(1, "a.bin")).await.unwrap();
        let data: Vec<u8> = (0..=255u8).collect();
        c.call(TWRITE, &write(1, 0, &data)).await.unwrap();
        c.call(TCLUNK, &1u32.to_le_bytes()).await.unwrap();
        assert_eq!(std::fs::read(root.join("a.bin")).unwrap(), data);
        c.call(TWALK, &walk(0, 2, &["a.bin"])).await.unwrap();
        let mut o = Wr::default();
        o.u32(2).u32(0);
        c.call(TLOPEN, &o.0).await.unwrap();
        let got = c.call(TREAD, &read(2, 10, 20)).await.unwrap();
        assert_eq!(&got[4..], &data[10..30]);
        let mut g = Wr::default();
        g.u32(2).u64(0x7ff);
        let a = c.call(TGETATTR, &g.0).await.unwrap();
        // valid[8] qid[13] mode[4] uid[4] gid[4]
        assert_eq!(u32::from_le_bytes(a[21..25].try_into().unwrap()) & 0o777, 0o644);
        assert_eq!(u32::from_le_bytes(a[25..29].try_into().unwrap()), 1234);
        assert_eq!(u32::from_le_bytes(a[29..33].try_into().unwrap()), 5678);
        // a directory listing has . .. and the file
        let mut rdir = Wr::default();
        rdir.u32(0).u32(0);
        c.call(TLOPEN, &rdir.0).await.unwrap();
        let mut rd = Wr::default();
        rd.u32(0).u64(0).u32(4096);
        let list = c.call(TREADDIR, &rd.0).await.unwrap();
        let text = String::from_utf8_lossy(&list[4..]).to_string();
        assert!(text.contains("a.bin") && text.contains('.'));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn names_a_host_could_not_hold_are_refused() {
        let root = tmp("names");
        let mut c = start(&root, 1 << 20).await;
        c.call(TWALK, &walk(0, 1, &[])).await.unwrap();
        for bad in ["a:b", "x/y", "CON", "nul.txt", "COM1", "trail.", "sp ", "q?", "tab\t", ".."] {
            c.call(TWALK, &walk(0, 1, &[])).await.unwrap();
            assert_eq!(c.call(TLCREATE, &create(1, bad)).await, Err(EINVAL), "{bad}");
        }
        c.call(TWALK, &walk(0, 1, &[])).await.unwrap();
        c.call(TLCREATE, &create(1, "Report.TXT")).await.unwrap();
        c.call(TWALK, &walk(0, 3, &[])).await.unwrap();
        assert_eq!(c.call(TLCREATE, &create(3, "report.txt")).await, Err(EEXIST), "other letters of an existing name");
        assert!(c.call(TWALK, &walk(0, 4, &["lpt0"])).await.is_err());
        assert!(check_name("COM0").is_ok() && check_name("coM9").is_err() && check_name("console").is_ok());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_quota_is_shared_and_freed_again() {
        let root = tmp("quota");
        let mut c1 = start(&root, 100).await;
        let mut c2 = start(&root, 100).await;
        c1.call(TWALK, &walk(0, 1, &[])).await.unwrap();
        c1.call(TLCREATE, &create(1, "one")).await.unwrap();
        c1.call(TWRITE, &write(1, 0, &[1; 60])).await.unwrap();
        c2.call(TWALK, &walk(0, 1, &[])).await.unwrap();
        c2.call(TLCREATE, &create(1, "two")).await.unwrap();
        assert_eq!(c2.call(TWRITE, &write(1, 0, &[2; 60])).await, Err(ENOSPC), "the other VM's bytes count");
        c2.call(TWRITE, &write(1, 0, &[2; 40])).await.unwrap();
        // rewriting does not grow
        c1.call(TWRITE, &write(1, 0, &[3; 60])).await.unwrap();
        // deleting frees
        let mut u = Wr::default();
        u.u32(0).str("one").u32(0);
        c2.call(TUNLINKAT, &u.0).await.unwrap();
        c2.call(TWRITE, &write(1, 40, &[2; 60])).await.unwrap();
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn directories_rename_and_remove_work_and_dotdot_stops_at_the_root() {
        let root = tmp("dirs");
        let mut c = start(&root, 1 << 20).await;
        let mut m = Wr::default();
        m.u32(0).str("sub").u32(0o755).u32(0);
        c.call(TMKDIR, &m.0).await.unwrap();
        c.call(TWALK, &walk(0, 1, &["sub"])).await.unwrap();
        c.call(TLCREATE, &create(1, "f")).await.unwrap();
        c.call(TWALK, &walk(0, 2, &["sub"])).await.unwrap();
        let mut rm = Wr::default();
        rm.u32(0).str("sub").u32(AT_REMOVEDIR);
        assert_eq!(c.call(TUNLINKAT, &rm.0).await, Err(ENOTEMPTY));
        let mut mv = Wr::default();
        mv.u32(2).str("f").u32(0).str("moved");
        c.call(TRENAMEAT, &mv.0).await.unwrap();
        assert!(root.join("moved").is_file() && !root.join("sub/f").exists());
        c.call(TUNLINKAT, &rm.0).await.unwrap();
        // walking up from the root stays at the root
        c.call(TWALK, &walk(0, 5, &["..", "..", "moved"])).await.unwrap();
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread")]
    async fn symlinks_are_never_followed_or_made() {
        let root = tmp("sym");
        let outside = tmp("sym-outside");
        std::fs::write(outside.join("secret"), "x").unwrap();
        std::os::unix::fs::symlink(&outside, root.join("link")).unwrap();
        let mut c = start(&root, 1 << 20).await;
        assert!(c.call(TWALK, &walk(0, 1, &["link"])).await.is_err());
        assert!(c.call(TWALK, &walk(0, 1, &["link", "secret"])).await.is_err());
        let mut s = Wr::default();
        s.u32(0).str("l2").str("/etc").u32(0);
        assert_eq!(c.call(TSYMLINK, &s.0).await, Err(EPERM));
        std::fs::remove_dir_all(&root).unwrap();
        std::fs::remove_dir_all(&outside).unwrap();
    }

    #[test]
    fn the_user_side_stays_inside_the_folder_and_keeps_the_quota() {
        let root = tmp("user");
        let outside = tmp("user-out");
        let q = quota_for(&root, 100);
        let src = outside.join("up.txt");
        std::fs::write(&src, [7u8; 60]).unwrap();
        make_dir(&root, "", "docs").unwrap();
        import_file(&root, "docs", &src, &q).unwrap();
        assert_eq!(std::fs::read(root.join("docs/up.txt")).unwrap().len(), 60);
        assert!(import_file(&root, "docs", &src, &q).is_err(), "no overwrite");
        std::fs::write(outside.join("big.bin"), [1u8; 60]).unwrap();
        assert!(import_file(&root, "", &outside.join("big.bin"), &q).unwrap_err().contains("quota"));
        let l = list(&root, "").unwrap();
        assert_eq!(l.len(), 1);
        assert!(l[0].dir && l[0].name == "docs");
        assert_eq!(list(&root, "docs").unwrap()[0].size, 60);
        for bad in ["..", "../x", "docs/../..", "a\\b"] {
            assert!(list(&root, bad).is_err(), "{bad}");
            assert!(remove(&root, bad, &q).is_err(), "{bad}");
        }
        assert!(make_dir(&root, "", "a:b").is_err() && make_dir(&root, "", "DOCS").is_err());
        let dest = outside.join("saved.txt");
        export(&root, "docs/up.txt", &dest).unwrap();
        assert_eq!(std::fs::read(&dest).unwrap().len(), 60);
        export(&root, "docs", &outside).unwrap();
        assert!(outside.join("docs/up.txt").is_file());
        remove(&root, "docs", &q).unwrap();
        assert_eq!(q.used(), 0, "deleting gives the bytes back");
        assert!(remove(&root, "", &q).is_err());
        std::fs::remove_dir_all(&root).unwrap();
        std::fs::remove_dir_all(&outside).unwrap();
    }

    /// By hand, with a VM: `P9_SOCK=/tmp/x/s P9_DIR=/some/dir cargo test serve_for_a_vm -- --ignored`
    /// serves the folder on a unix socket (QEMU connects with `-chardev socket,path=...`) for 80 s
    /// (`P9_SECS` changes that).
    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread")]
    #[ignore]
    async fn serve_for_a_vm() {
        let (sock, dir) = (std::env::var("P9_SOCK").unwrap(), PathBuf::from(std::env::var("P9_DIR").unwrap()));
        let _ = std::fs::remove_file(&sock);
        let l = tokio::net::UnixListener::bind(&sock).unwrap();
        let (s, _) = l.accept().await.unwrap();
        let q = quota_for(&dir, 1 << 30);
        let _ = tokio::time::timeout(std::time::Duration::from_secs(std::env::var("P9_SECS").ok().and_then(|v| v.parse().ok()).unwrap_or(80)), serve(Box::new(s), dir, q)).await;
    }
}
