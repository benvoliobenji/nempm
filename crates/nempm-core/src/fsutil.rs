//! File operations that have to survive a USB stick being pulled: staged
//! writes, real flushes, and read-back that bypasses the OS cache.

use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::Result;
use crate::error::io_ctx;
use crate::progress::CancelToken;

pub const CHUNK: usize = 1 << 20;

/// Suffix for files being written; Check drive offers to clean these up.
pub const PART_SUFFIX: &str = ".nempm-part";
/// The suffix used before the app was called nempm.
pub const LEGACY_PART_SUFFIX: &str = ".pkgmgr-part";

pub fn part_path(dest: &Path) -> PathBuf {
    let mut name = dest.file_name().unwrap_or_default().to_os_string();
    name.push(PART_SUFFIX);
    dest.with_file_name(name)
}

/// Flushes file data to the device. On macOS `fsync` only reaches the drive's
/// cache, so `F_FULLFSYNC` is used there.
pub fn full_sync(file: &File) -> io::Result<()> {
    #[cfg(target_os = "macos")]
    {
        use std::os::fd::AsRawFd;
        // SAFETY: valid descriptor for the lifetime of `file`.
        if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_FULLFSYNC) } == 0 {
            return Ok(());
        }
    }
    file.sync_all()
}

/// Makes a rename durable by syncing the containing folder (Unix only;
/// Windows has no equivalent for directories).
pub fn sync_dir(dir: &Path) {
    #[cfg(unix)]
    if let Ok(f) = File::open(dir) {
        let _ = f.sync_all();
    }
    #[cfg(not(unix))]
    let _ = dir;
}

/// Writes `data` to `path` via `<path>.tmp` + fsync + rename, so a reader
/// sees either the old file or the new one, never half of either.
pub fn write_atomic(path: &Path, data: &[u8]) -> io::Result<()> {
    let mut tmp_name = path.file_name().unwrap_or_default().to_os_string();
    tmp_name.push(".tmp");
    let tmp = path.with_file_name(tmp_name);
    {
        let mut f = File::create(&tmp)?;
        f.write_all(data)?;
        full_sync(&f)?;
    }
    if let Err(e) = fs::rename(&tmp, path) {
        let _ = fs::remove_file(&tmp);
        return Err(e);
    }
    if let Some(parent) = path.parent() {
        sync_dir(parent);
    }
    Ok(())
}

pub fn remove_file_if_exists(path: &Path) -> io::Result<bool> {
    match fs::remove_file(path) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e),
    }
}

/// Bytes available to this user on the volume holding `path`.
pub fn free_space(path: &Path) -> io::Result<u64> {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
        let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        let mut free = 0u64;
        // SAFETY: `wide` is NUL-terminated; out pointers are valid or null.
        let ok = unsafe { GetDiskFreeSpaceExW(wide.as_ptr(), &mut free, std::ptr::null_mut(), std::ptr::null_mut()) };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(free)
    }
    #[cfg(unix)]
    {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;
        let c = CString::new(path.as_os_str().as_bytes()).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
        // SAFETY: zeroed statvfs is a valid out-parameter.
        let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
        // SAFETY: `c` is NUL-terminated and `stat` is writable.
        if unsafe { libc::statvfs(c.as_ptr(), &mut stat) } != 0 {
            return Err(io::Error::last_os_error());
        }
        #[allow(clippy::unnecessary_cast)]
        Ok(stat.f_bavail as u64 * stat.f_frsize as u64)
    }
}

/// Opens a file for reading so the bytes come from the device, not the page
/// cache, where the OS allows it. Used for read-back verification: cheap USB
/// sticks fail silently and a cached read would hide that.
fn open_uncached(path: &Path) -> io::Result<(File, bool)> {
    #[cfg(windows)]
    {
        use std::fs::OpenOptions;
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_NO_BUFFERING;
        if let Ok(f) = OpenOptions::new()
            .read(true)
            .custom_flags(FILE_FLAG_NO_BUFFERING)
            .open(path)
        {
            return Ok((f, true));
        }
        Ok((File::open(path)?, false))
    }
    #[cfg(target_os = "linux")]
    {
        use std::os::fd::AsRawFd;
        let f = File::open(path)?;
        // SAFETY: valid descriptor. Drops clean cached pages for this file.
        unsafe {
            libc::posix_fadvise(f.as_raw_fd(), 0, 0, libc::POSIX_FADV_DONTNEED);
        }
        Ok((f, false))
    }
    #[cfg(target_os = "macos")]
    {
        use std::os::fd::AsRawFd;
        let f = File::open(path)?;
        // SAFETY: valid descriptor.
        unsafe {
            libc::fcntl(f.as_raw_fd(), libc::F_NOCACHE, 1);
        }
        Ok((f, false))
    }
    #[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
    {
        Ok((File::open(path)?, false))
    }
}

/// A buffer aligned to 4096 bytes, as unbuffered reads on Windows require.
struct AlignedBuf {
    storage: Vec<u8>,
    offset: usize,
}

impl AlignedBuf {
    const ALIGN: usize = 4096;

    fn new(len: usize) -> Self {
        let storage = vec![0u8; len + Self::ALIGN];
        let offset = storage.as_ptr().align_offset(Self::ALIGN);
        Self { storage, offset }
    }

    fn as_mut(&mut self) -> &mut [u8] {
        let len = self.storage.len() - Self::ALIGN;
        &mut self.storage[self.offset..self.offset + len]
    }
}

/// SHA-256 and length of a file. `uncached` reads straight from the device
/// where possible (read-back verification).
pub fn hash_file(
    path: &Path,
    uncached: bool,
    mut on_bytes: impl FnMut(u64),
    cancel: &CancelToken,
) -> Result<(String, u64)> {
    let ctx = || format!("reading {}", path.display());
    let (file, unbuffered) = if uncached {
        open_uncached(path).map_err(io_ctx(ctx()))?
    } else {
        (File::open(path).map_err(io_ctx(ctx()))?, false)
    };
    match hash_reader(file, &mut on_bytes, cancel) {
        // Unbuffered I/O can reject a device's sector size; fall back.
        Err(crate::Error::Io { source, .. }) if unbuffered && source.raw_os_error() == Some(87) => {
            hash_reader(File::open(path).map_err(io_ctx(ctx()))?, &mut on_bytes, cancel)
        }
        other => other.map_err(|e| match e {
            crate::Error::Io { source, .. } => crate::error::io_error(ctx(), source),
            e => e,
        }),
    }
}

fn hash_reader(mut file: File, on_bytes: &mut impl FnMut(u64), cancel: &CancelToken) -> Result<(String, u64)> {
    let mut hasher = Sha256::new();
    let mut buf = AlignedBuf::new(CHUNK);
    let mut total = 0u64;
    loop {
        cancel.check()?;
        let n = match file.read(buf.as_mut()) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => {
                return Err(crate::Error::Io {
                    context: String::new(),
                    source: e,
                });
            }
        };
        hasher.update(&buf.as_mut()[..n]);
        total += n as u64;
        on_bytes(total);
    }
    Ok((hex::encode(hasher.finalize()), total))
}

/// Copies `src` to `dst` (truncating it), hashing what is written, then
/// flushes it to the device. Returns the SHA-256 and byte count.
pub fn copy_hashed(
    src: &Path,
    dst: &Path,
    mut on_bytes: impl FnMut(u64),
    cancel: &CancelToken,
) -> Result<(String, u64)> {
    let mut input = File::open(src).map_err(io_ctx(format!("reading {}", src.display())))?;
    let write_ctx = || format!("writing {}", dst.display());
    let mut output = File::create(dst).map_err(io_ctx(write_ctx()))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; CHUNK];
    let mut total = 0u64;
    loop {
        cancel.check()?;
        let n = match input.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(crate::error::io_error(format!("reading {}", src.display()), e)),
        };
        output.write_all(&buf[..n]).map_err(io_ctx(write_ctx()))?;
        hasher.update(&buf[..n]);
        total += n as u64;
        on_bytes(total);
    }
    full_sync(&output).map_err(io_ctx(write_ctx()))?;
    Ok((hex::encode(hasher.finalize()), total))
}

/// Writes everything from `reader` to `dst`, hashing it, then flushes. Used
/// for zip entries.
pub fn write_hashed(
    reader: &mut impl Read,
    dst: &Path,
    mut on_bytes: impl FnMut(u64),
    cancel: &CancelToken,
) -> Result<(String, u64)> {
    let write_ctx = || format!("writing {}", dst.display());
    let mut output = File::create(dst).map_err(io_ctx(write_ctx()))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; CHUNK];
    let mut total = 0u64;
    loop {
        cancel.check()?;
        let n = match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(crate::Error::InvalidData(format!("The archive is damaged: {e}"))),
        };
        output.write_all(&buf[..n]).map_err(io_ctx(write_ctx()))?;
        hasher.update(&buf[..n]);
        total += n as u64;
        on_bytes(n as u64);
    }
    full_sync(&output).map_err(io_ctx(write_ctx()))?;
    Ok((hex::encode(hasher.finalize()), total))
}

/// Removes `start` and then each parent, while empty, stopping at `stop` (not
/// removed). Errors are ignored: a folder that isn't empty stays.
pub fn prune_empty_dirs(start: &Path, stop: &Path) {
    let mut dir = start.to_path_buf();
    while dir.starts_with(stop) && dir != stop {
        if fs::remove_dir(&dir).is_err() {
            break;
        }
        match dir.parent() {
            Some(p) => dir = p.to_path_buf(),
            None => break,
        }
    }
}

/// A unique-enough suffix for staging names.
pub fn nonce() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    format!("{:x}{:x}", std::process::id(), nanos)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_known_value() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.txt");
        fs::write(&path, b"abc").unwrap();
        let (sha, len) = hash_file(&path, false, |_| {}, &CancelToken::new()).unwrap();
        assert_eq!(sha, "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        assert_eq!(len, 3);
        let (sha2, _) = hash_file(&path, true, |_| {}, &CancelToken::new()).unwrap();
        assert_eq!(sha, sha2);
    }

    #[test]
    fn uncached_read_handles_sizes_that_are_not_sector_multiples() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("odd.bin");
        let data: Vec<u8> = (0..(CHUNK + 4097)).map(|i| (i % 251) as u8).collect();
        fs::write(&path, &data).unwrap();
        let (sha, len) = hash_file(&path, true, |_| {}, &CancelToken::new()).unwrap();
        assert_eq!(len, data.len() as u64);
        assert_eq!(sha, hex::encode(Sha256::digest(&data)));
    }

    #[test]
    fn copy_and_atomic_write() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        fs::write(&src, b"hello").unwrap();
        let dst = part_path(&dir.path().join("dst.zip"));
        assert!(dst.to_string_lossy().ends_with("dst.zip.nempm-part"));
        let (sha, n) = copy_hashed(&src, &dst, |_| {}, &CancelToken::new()).unwrap();
        assert_eq!(n, 5);
        assert_eq!(sha, hex::encode(Sha256::digest(b"hello")));

        let manifest = dir.path().join("m.json");
        write_atomic(&manifest, b"{}").unwrap();
        write_atomic(&manifest, b"{\"a\":1}").unwrap();
        assert_eq!(fs::read(&manifest).unwrap(), b"{\"a\":1}");
        assert!(!dir.path().join("m.json.tmp").exists());
    }

    #[test]
    fn cancelled_copy_stops() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        fs::write(&src, b"hello").unwrap();
        let token = CancelToken::new();
        token.cancel();
        let res = copy_hashed(&src, &dir.path().join("dst"), |_| {}, &token);
        assert!(matches!(res, Err(crate::Error::Cancelled)));
    }

    #[test]
    fn prune_stops_at_boundary() {
        let dir = tempfile::tempdir().unwrap();
        let deep = dir.path().join("a/b/c");
        fs::create_dir_all(&deep).unwrap();
        prune_empty_dirs(&deep, &dir.path().join("a"));
        assert!(dir.path().join("a").exists());
        assert!(!dir.path().join("a/b").exists());
    }

    #[test]
    fn free_space_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        assert!(free_space(dir.path()).unwrap() > 0);
    }
}
