//! [`SafeDir`] on Windows through handle-relative NT calls (see the parent module).

use std::io::{self, ErrorKind};
use std::marker::PhantomData;
use std::mem::size_of;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle, RawHandle};
use std::path::{Path, PathBuf};
use std::ptr::null_mut;

use windows_sys::Wdk::Foundation::OBJECT_ATTRIBUTES;
use windows_sys::Wdk::Storage::FileSystem::{
    FileDispositionInformationEx, FileLinkInformation, FileRenameInformationEx, NtCreateFile, NtSetInformationFile,
    FILE_CREATE, FILE_DIRECTORY_FILE, FILE_DISPOSITION_DELETE, FILE_DISPOSITION_IGNORE_READONLY_ATTRIBUTE,
    FILE_DISPOSITION_POSIX_SEMANTICS, FILE_LINK_INFORMATION, FILE_NON_DIRECTORY_FILE, FILE_OPEN,
    FILE_OPEN_FOR_BACKUP_INTENT, FILE_OPEN_REPARSE_POINT, FILE_RENAME_INFORMATION, FILE_RENAME_POSIX_SEMANTICS,
    FILE_RENAME_REPLACE_IF_EXISTS, FILE_SYNCHRONOUS_IO_NONALERT, NTCREATEFILE_CREATE_DISPOSITION,
    NTCREATEFILE_CREATE_OPTIONS,
};
use windows_sys::Win32::Foundation::{
    CloseHandle, DuplicateHandle, GetLastError, RtlNtStatusToDosError, DUPLICATE_SAME_ACCESS, HANDLE, NTSTATUS,
    OBJ_CASE_INSENSITIVE, STATUS_NOT_A_DIRECTORY, STATUS_OBJECT_NAME_COLLISION, STATUS_OBJECT_NAME_INVALID,
    STATUS_OBJECT_NAME_NOT_FOUND, STATUS_OBJECT_PATH_NOT_FOUND, UNICODE_STRING, WAIT_ABANDONED, WAIT_OBJECT_0,
};
use windows_sys::Win32::Storage::FileSystem::{
    FileFullDirectoryInfo, FileFullDirectoryRestartInfo, FlushFileBuffers, GetFileInformationByHandle,
    GetFileInformationByHandleEx, ReadFile, WriteFile, BY_HANDLE_FILE_INFORMATION, DELETE, FILE_ATTRIBUTE_DIRECTORY,
    FILE_ATTRIBUTE_NORMAL, FILE_ATTRIBUTE_READONLY, FILE_ATTRIBUTE_REPARSE_POINT, FILE_FULL_DIR_INFO,
    FILE_GENERIC_EXECUTE, FILE_GENERIC_READ, FILE_GENERIC_WRITE, FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE,
    FILE_SHARE_MODE, FILE_SHARE_READ, FILE_SHARE_WRITE, SYNCHRONIZE,
};
use windows_sys::Win32::System::Threading::{
    CreateMutexW, GetCurrentProcess, ReleaseMutex, WaitForSingleObject, INFINITE,
};
use windows_sys::Win32::System::IO::IO_STATUS_BLOCK;

use super::{cname, Entry, Result, SafeError};

/// POSIX file-type bits, so [`Entry::mode`] reads the same on every platform.
const S_IFMT: u32 = 0o170000;
const S_IFREG: u32 = 0o100000;
const S_IFDIR: u32 = 0o040000;
const S_IFLNK: u32 = 0o120000;

const MAX_DEPTH: usize = 8;

/// Access needed on directory handles used as `RootDirectory` for child opens.
const DIR_ACCESS: u32 =
    FILE_GENERIC_READ | FILE_GENERIC_WRITE | FILE_GENERIC_EXECUTE | FILE_READ_ATTRIBUTES | SYNCHRONIZE;
const FILE_WRITE_ACCESS: u32 = FILE_GENERIC_WRITE | FILE_GENERIC_READ | SYNCHRONIZE;

fn unsafe_at(p: &str, why: &str) -> SafeError {
    SafeError::Unsafe(format!("{p}: {why}"))
}

fn nt_absolute_path(canonical: &Path) -> String {
    let s = canonical.to_string_lossy();
    if let Some(rest) = s.strip_prefix(r"\\?\") {
        format!(r"\??\{rest}")
    } else {
        format!(r"\??\{}", canonical.display())
    }
}

fn display_path(path: PathBuf) -> PathBuf {
    let raw = path.to_string_lossy();
    if let Some(rest) = raw.strip_prefix(r"\\?\") {
        let bytes = rest.as_bytes();
        if rest.len() >= 2 && bytes.get(1) == Some(&b':') {
            return PathBuf::from(rest);
        }
    }
    path
}

fn filetime_to_unix(high: u32, low: u32) -> (i64, i64) {
    let ticks = ((high as u64) << 32) | low as u64;
    const EPOCH: u64 = 116_444_736_000_000_000;
    let since = ticks.saturating_sub(EPOCH);
    let secs = (since / 10_000_000) as i64;
    let nsec = ((since % 10_000_000) * 100) as i64;
    (secs, nsec)
}

fn wide_name(name: &str) -> io::Result<Vec<u16>> {
    Ok(name.encode_utf16().collect())
}

struct NtName {
    wide: Vec<u16>,
}

impl NtName {
    fn new(name: &str) -> io::Result<Self> {
        Ok(Self { wide: wide_name(name)? })
    }

    fn unicode(&mut self) -> UNICODE_STRING {
        let byte_len =
            u16::try_from(self.wide.len().checked_mul(2).expect("name length")).expect("name fits UNICODE_STRING");
        let max_len = byte_len.saturating_add(2);
        UNICODE_STRING { Length: byte_len, MaximumLength: max_len, Buffer: self.wide.as_mut_ptr() }
    }
}

fn nt_status_error(status: NTSTATUS) -> io::Error {
    if status == STATUS_OBJECT_NAME_NOT_FOUND || status == STATUS_OBJECT_PATH_NOT_FOUND {
        return io::Error::from(ErrorKind::NotFound);
    }
    if status == STATUS_OBJECT_NAME_COLLISION {
        return io::Error::from(ErrorKind::AlreadyExists);
    }
    // SAFETY: RtlNtStatusToDosError maps known NTSTATUS values to Win32 error codes.
    let code = unsafe { RtlNtStatusToDosError(status) };
    io::Error::from_raw_os_error(code as i32)
}

fn by_handle(handle: RawHandle) -> io::Result<BY_HANDLE_FILE_INFORMATION> {
    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    // SAFETY: `info` matches the layout expected by GetFileInformationByHandle.
    let ok = unsafe { GetFileInformationByHandle(handle, &mut info) };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(info)
}

fn open_root(path: &Path) -> io::Result<OwnedHandle> {
    let canonical = path.canonicalize()?;
    let abs = nt_absolute_path(&canonical);
    nt_create(
        null_mut(),
        &abs,
        DIR_ACCESS,
        share_all(),
        FILE_OPEN,
        FILE_DIRECTORY_FILE | FILE_OPEN_REPARSE_POINT | FILE_OPEN_FOR_BACKUP_INTENT,
    )
}

fn nt_create_status(
    parent: RawHandle,
    name: &str,
    access: u32,
    share: FILE_SHARE_MODE,
    disposition: NTCREATEFILE_CREATE_DISPOSITION,
    options: NTCREATEFILE_CREATE_OPTIONS,
    attributes: u32,
) -> std::result::Result<OwnedHandle, NTSTATUS> {
    let mut nt = NtName::new(name).map_err(|_| STATUS_OBJECT_NAME_INVALID)?;
    let unicode = nt.unicode();
    let oa = OBJECT_ATTRIBUTES {
        Length: u32::try_from(size_of::<OBJECT_ATTRIBUTES>()).expect("OBJECT_ATTRIBUTES size"),
        RootDirectory: parent as HANDLE,
        ObjectName: std::ptr::addr_of!(unicode),
        Attributes: OBJ_CASE_INSENSITIVE,
        SecurityDescriptor: null_mut(),
        SecurityQualityOfService: null_mut(),
    };
    let mut iosb = IO_STATUS_BLOCK::default();
    let mut handle = null_mut();
    let zero_size: i64 = 0;
    // SAFETY: `oa` and `unicode` are valid for the duration of the call; `handle` is an out-pointer.
    let status = unsafe {
        NtCreateFile(
            &mut handle,
            access,
            &raw const oa,
            &mut iosb,
            &raw const zero_size,
            attributes,
            share,
            disposition,
            options,
            null_mut(),
            0,
        )
    };
    if status < 0 {
        return Err(status);
    }
    // SAFETY: NtCreateFile succeeded and returned an owning handle.
    Ok(unsafe { OwnedHandle::from_raw_handle(handle) })
}

fn nt_create(
    parent: RawHandle,
    name: &str,
    access: u32,
    share: FILE_SHARE_MODE,
    disposition: NTCREATEFILE_CREATE_DISPOSITION,
    options: NTCREATEFILE_CREATE_OPTIONS,
) -> io::Result<OwnedHandle> {
    nt_create_status(parent, name, access, share, disposition, options, 0).map_err(nt_status_error)
}

fn dup_handle(handle: RawHandle) -> io::Result<OwnedHandle> {
    let mut out = null_mut();
    // SAFETY: both processes are the current process; `out` receives a duplicated handle.
    let ok = unsafe {
        DuplicateHandle(
            GetCurrentProcess(),
            handle as HANDLE,
            GetCurrentProcess(),
            &mut out,
            0,
            0,
            DUPLICATE_SAME_ACCESS,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: DuplicateHandle succeeded.
    Ok(unsafe { OwnedHandle::from_raw_handle(out) })
}

fn share_all() -> FILE_SHARE_MODE {
    FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE
}

fn write_all_handle(handle: RawHandle, bytes: &[u8]) -> io::Result<()> {
    let mut offset = 0usize;
    while offset < bytes.len() {
        let mut written = 0u32;
        let to_write = bytes.len().saturating_sub(offset);
        let chunk = u32::try_from(to_write.min(u32::MAX as usize)).expect("write chunk");
        // SAFETY: `handle` is a writable file handle; buffer outlives the call.
        let ok = unsafe { WriteFile(handle, bytes.as_ptr().add(offset), chunk, &mut written, null_mut()) };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        offset += usize::try_from(written).expect("bytes written");
    }
    // SAFETY: `handle` is a file handle we opened for write.
    let flushed = unsafe { FlushFileBuffers(handle) };
    if flushed == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn read_all_handle(handle: RawHandle, max: usize) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        let mut n = 0u32;
        let cap = max.saturating_sub(bytes.len()).min(buf.len());
        if cap == 0 {
            break;
        }
        // SAFETY: `handle` was opened for synchronous read; buffer outlives the call.
        let ok = unsafe { ReadFile(handle, buf.as_mut_ptr(), u32::try_from(cap).expect("cap"), &mut n, null_mut()) };
        if ok == 0 {
            let err = io::Error::last_os_error();
            if err.kind() == ErrorKind::UnexpectedEof || err.raw_os_error() == Some(109_i32) {
                break;
            }
            return Err(err);
        }
        if n == 0 {
            break;
        }
        bytes.extend_from_slice(&buf[..n as usize]);
        if bytes.len() > max {
            return Err(io::Error::new(ErrorKind::InvalidData, "read past limit"));
        }
    }
    Ok(bytes)
}

fn is_reparse(attrs: u32) -> bool {
    attrs & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

fn is_directory(attrs: u32) -> bool {
    attrs & FILE_ATTRIBUTE_DIRECTORY != 0
}

fn mode_from_attrs(attrs: u32) -> u32 {
    if is_reparse(attrs) {
        return S_IFLNK | 0o777;
    }
    if is_directory(attrs) {
        return S_IFDIR | 0o755;
    }
    if attrs & FILE_ATTRIBUTE_READONLY != 0 {
        return S_IFREG | 0o444;
    }
    S_IFREG | 0o644
}

fn entry_from_info(name: String, info: &BY_HANDLE_FILE_INFORMATION) -> Entry {
    let (mtime, mtime_nsec) = filetime_to_unix(info.ftLastWriteTime.dwHighDateTime, info.ftLastWriteTime.dwLowDateTime);
    let size = ((info.nFileSizeHigh as u64) << 32) | info.nFileSizeLow as u64;
    Entry { name, mode: mode_from_attrs(info.dwFileAttributes), size, mtime, mtime_nsec }
}

fn nt_set_information(handle: RawHandle, class: i32, buf: *const u8, len: u32) -> io::Result<()> {
    let mut iosb = IO_STATUS_BLOCK::default();
    // SAFETY: caller sized `buf`/`len` for `class`; iosb is a valid out-status block.
    let status = unsafe { NtSetInformationFile(handle as HANDLE, &mut iosb, buf.cast(), len, class) };
    if status < 0 {
        return Err(nt_status_error(status));
    }
    Ok(())
}

fn delete_by_handle(handle: OwnedHandle) -> io::Result<()> {
    let info = windows_sys::Wdk::Storage::FileSystem::FILE_DISPOSITION_INFORMATION_EX {
        Flags: FILE_DISPOSITION_DELETE | FILE_DISPOSITION_POSIX_SEMANTICS | FILE_DISPOSITION_IGNORE_READONLY_ATTRIBUTE,
    };
    let len = u32::try_from(size_of::<windows_sys::Wdk::Storage::FileSystem::FILE_DISPOSITION_INFORMATION_EX>())
        .expect("disposition struct size");
    // SAFETY: `info` is a properly aligned FILE_DISPOSITION_INFORMATION_EX.
    nt_set_information(handle.as_raw_handle(), FileDispositionInformationEx, &raw const info as *const u8, len)?;
    Ok(())
}

fn name_info_buffer<T>(wide: &[u16]) -> io::Result<Vec<u8>> {
    let name_bytes =
        wide.len().checked_mul(2).ok_or_else(|| io::Error::new(ErrorKind::InvalidInput, "name too long"))?;
    let size = size_of::<T>()
        .checked_sub(2)
        .and_then(|h| h.checked_add(name_bytes))
        .ok_or_else(|| io::Error::new(ErrorKind::InvalidInput, "name too long"))?;
    Ok(vec![0u8; size])
}

/// Opens the directory `name` inside `parent`, never through a junction or symlink.
fn open_dir_at(parent: RawHandle, name: &str, shown: &str) -> Result<Option<OwnedHandle>> {
    let _c = cname(name)?;
    let opts = FILE_OPEN_REPARSE_POINT | FILE_OPEN_FOR_BACKUP_INTENT | FILE_DIRECTORY_FILE;
    match nt_create_status(parent, name, DIR_ACCESS, share_all(), FILE_OPEN, opts, 0) {
        Ok(handle) => {
            let info = by_handle(handle.as_raw_handle()).map_err(SafeError::from)?;
            if is_reparse(info.dwFileAttributes) {
                drop(handle);
                return Err(unsafe_at(shown, "is a symlink"));
            }
            Ok(Some(handle))
        }
        Err(STATUS_OBJECT_NAME_NOT_FOUND) | Err(STATUS_OBJECT_PATH_NOT_FOUND) => Ok(None),
        Err(STATUS_NOT_A_DIRECTORY) => classify_not_dir(parent, name, shown),
        Err(status) => Err(nt_status_error(status).into()),
    }
}

fn classify_not_dir(parent: RawHandle, name: &str, shown: &str) -> Result<Option<OwnedHandle>> {
    let opts = FILE_OPEN_REPARSE_POINT | FILE_OPEN_FOR_BACKUP_INTENT | FILE_READ_ATTRIBUTES;
    match nt_create_status(parent, name, FILE_READ_ATTRIBUTES, share_all(), FILE_OPEN, opts, 0) {
        Ok(handle) => {
            let info = by_handle(handle.as_raw_handle()).map_err(SafeError::from)?;
            drop(handle);
            let why = if is_reparse(info.dwFileAttributes) { "is a symlink" } else { "is not a directory" };
            Err(unsafe_at(shown, why))
        }
        Err(STATUS_OBJECT_NAME_NOT_FOUND) | Err(STATUS_OBJECT_PATH_NOT_FOUND) => Ok(None),
        Err(status) => Err(nt_status_error(status).into()),
    }
}

fn mkdir_at(parent: RawHandle, name: &str) -> Result<bool> {
    let _c = cname(name)?;
    let opts = FILE_OPEN_REPARSE_POINT | FILE_OPEN_FOR_BACKUP_INTENT | FILE_DIRECTORY_FILE;
    match nt_create(parent, name, DIR_ACCESS, share_all(), FILE_CREATE, opts) {
        Ok(handle) => {
            drop(handle);
            Ok(true)
        }
        Err(e) if e.kind() == ErrorKind::AlreadyExists => Ok(false),
        Err(e) => Err(e.into()),
    }
}

impl Entry {
    pub fn is_file(&self) -> bool {
        self.mode & S_IFMT == S_IFREG
    }

    pub fn is_dir(&self) -> bool {
        self.mode & S_IFMT == S_IFDIR
    }
}

/// An exclusive lock on a directory, released on drop (`!Send`: the mutex is thread-affine).
pub struct Lock {
    handle: HANDLE,
    _not_send: PhantomData<*const ()>,
}

impl Drop for Lock {
    fn drop(&mut self) {
        // SAFETY: we own the mutex and acquired it with WaitForSingleObject.
        unsafe {
            ReleaseMutex(self.handle);
            CloseHandle(self.handle);
        }
    }
}

/// A directory opened through handles (see the module docs).
#[derive(Debug)]
pub struct SafeDir {
    handle: OwnedHandle,
    volume_serial: u32,
    file_index: u64,
    path: PathBuf,
}

impl SafeDir {
    fn dir_identity(handle: RawHandle) -> io::Result<(u32, u64)> {
        let info = by_handle(handle)?;
        let index = ((info.nFileIndexHigh as u64) << 32) | info.nFileIndexLow as u64;
        Ok((info.dwVolumeSerialNumber, index))
    }

    fn walk(root: &Path, segs: &[&str], create: Option<u32>) -> Result<Option<SafeDir>> {
        let root = root.canonicalize().map_err(|e| match e.kind() {
            ErrorKind::NotFound => SafeError::Missing(root.display().to_string()),
            _ => e.into(),
        })?;
        let shown_root = display_path(root.clone());
        let mut handle = open_root(&root).map_err(SafeError::from)?;
        let (volume_serial, file_index) = Self::dir_identity(handle.as_raw_handle())?;
        let mut path = shown_root;
        for seg in segs {
            path.push(seg);
            let label = path.display().to_string();
            let mut next = open_dir_at(handle.as_raw_handle(), seg, &label)?;
            if next.is_none() && create.is_some() {
                mkdir_at(handle.as_raw_handle(), seg)?;
                next = open_dir_at(handle.as_raw_handle(), seg, &label)?;
            }
            match next {
                Some(n) => handle = n,
                None => return Ok(None),
            }
        }
        Ok(Some(SafeDir { handle, volume_serial, file_index, path }))
    }

    pub fn open(root: &Path, segs: &[&str]) -> Result<Option<SafeDir>> {
        Self::walk(root, segs, None)
    }

    pub fn create(root: &Path, segs: &[&str], mode: u32) -> Result<SafeDir> {
        let _ = mode;
        Self::walk(root, segs, Some(0))?.ok_or_else(|| SafeError::Io("could not create the directory".into()))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn raw(&self) -> RawHandle {
        self.handle.as_raw_handle()
    }

    pub fn try_clone(&self) -> Result<SafeDir> {
        Ok(SafeDir {
            handle: dup_handle(self.raw()).map_err(SafeError::from)?,
            volume_serial: self.volume_serial,
            file_index: self.file_index,
            path: self.path.clone(),
        })
    }

    fn shown(&self, name: &str) -> String {
        self.path.join(name).display().to_string()
    }

    pub fn open_dir(&self, name: &str) -> Result<Option<SafeDir>> {
        let shown = self.path.join(name);
        let Some(handle) = open_dir_at(self.raw(), name, &shown.display().to_string())? else {
            return Ok(None);
        };
        let (volume_serial, file_index) = Self::dir_identity(handle.as_raw_handle())?;
        Ok(Some(SafeDir { handle, volume_serial, file_index, path: shown }))
    }

    pub fn ensure_dir(&self, name: &str, mode: u32) -> Result<SafeDir> {
        let _ = mode;
        mkdir_at(self.raw(), name)?;
        self.open_dir(name)?.ok_or_else(|| SafeError::Missing(self.shown(name)))
    }

    pub fn create_dir_new(&self, name: &str, mode: u32) -> Result<SafeDir> {
        let _ = mode;
        if !mkdir_at(self.raw(), name)? {
            return Err(SafeError::Exists(name.to_string()));
        }
        self.open_dir(name)?.ok_or_else(|| SafeError::Missing(self.shown(name)))
    }

    pub fn entries(&self, limit: usize) -> Result<Vec<Entry>> {
        let mut buf = vec![0u8; 64 * 1024];
        let mut out = Vec::new();
        let mut restart = true;
        loop {
            let class = if restart { FileFullDirectoryRestartInfo } else { FileFullDirectoryInfo };
            restart = false;
            // SAFETY: buffer is large enough for at least one FILE_FULL_DIR_INFO record.
            let ok = unsafe {
                GetFileInformationByHandleEx(
                    self.raw(),
                    class,
                    buf.as_mut_ptr().cast(),
                    u32::try_from(buf.len()).expect("buffer length"),
                )
            };
            if ok == 0 {
                let err = io::Error::last_os_error();
                if matches!(err.raw_os_error(), Some(18) | Some(234)) {
                    break;
                }
                return Err(err.into());
            }
            let mut offset = 0usize;
            loop {
                if offset + size_of::<FILE_FULL_DIR_INFO>() > buf.len() {
                    break;
                }
                // SAFETY: the kernel filled valid FILE_FULL_DIR_INFO records in `buf`.
                let info = unsafe { &*(buf.as_ptr().add(offset) as *const FILE_FULL_DIR_INFO) };
                let name_len = usize::try_from(info.FileNameLength).expect("name length") / 2;
                let name_wide = unsafe { std::slice::from_raw_parts(info.FileName.as_ptr(), name_len) };
                let name = String::from_utf16_lossy(name_wide);
                if name != "." && name != ".." {
                    let attrs = info.FileAttributes;
                    let ticks = info.LastWriteTime as u64;
                    let (mtime, mtime_nsec) = filetime_to_unix((ticks >> 32) as u32, ticks as u32);
                    let size = info.EndOfFile as u64;
                    out.push(Entry { name, mode: mode_from_attrs(attrs), size, mtime, mtime_nsec });
                    if out.len() > limit {
                        return Ok(out);
                    }
                }
                if info.NextEntryOffset == 0 {
                    break;
                }
                offset += usize::try_from(info.NextEntryOffset).expect("offset");
            }
            if offset == 0 {
                break;
            }
        }
        Ok(out)
    }

    fn stat_open(&self, name: &str) -> Result<Option<(OwnedHandle, BY_HANDLE_FILE_INFORMATION)>> {
        let _c = cname(name)?;
        let opts = FILE_OPEN_REPARSE_POINT | FILE_OPEN_FOR_BACKUP_INTENT | FILE_READ_ATTRIBUTES;
        match nt_create(self.raw(), name, FILE_READ_ATTRIBUTES | FILE_GENERIC_READ, share_all(), FILE_OPEN, opts) {
            Ok(handle) => {
                let info = by_handle(handle.as_raw_handle())?;
                Ok(Some((handle, info)))
            }
            Err(e) if e.kind() == ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    pub fn stat(&self, name: &str) -> Result<Option<Entry>> {
        let Some((handle, info)) = self.stat_open(name)? else {
            return Ok(None);
        };
        drop(handle);
        if is_reparse(info.dwFileAttributes) {
            return Err(unsafe_at(&self.shown(name), "is a symlink"));
        }
        Ok(Some(entry_from_info(name.to_string(), &info)))
    }

    pub fn read_bytes(&self, name: &str, max: usize) -> Result<Vec<u8>> {
        let shown = self.shown(name);
        let _c = cname(name)?;
        match self.stat(name)? {
            None => return Err(SafeError::Missing(shown)),
            Some(ref entry) if !entry.is_file() => {
                return Err(unsafe_at(&shown, "is not a regular file"));
            }
            Some(_) => {}
        }
        let opts = FILE_OPEN_REPARSE_POINT | FILE_NON_DIRECTORY_FILE | FILE_SYNCHRONOUS_IO_NONALERT;
        let handle = match nt_create(self.raw(), name, FILE_GENERIC_READ, share_all(), FILE_OPEN, opts) {
            Ok(h) => h,
            Err(e) if e.kind() == ErrorKind::NotFound => return Err(SafeError::Missing(shown)),
            Err(e) => return Err(e.into()),
        };
        let info = by_handle(handle.as_raw_handle()).map_err(SafeError::from)?;
        if is_reparse(info.dwFileAttributes) {
            return Err(unsafe_at(&shown, "is a symlink"));
        }
        if is_directory(info.dwFileAttributes) {
            return Err(unsafe_at(&shown, "is not a regular file"));
        }
        if info.nNumberOfLinks != 1 || info.dwVolumeSerialNumber != self.volume_serial {
            return Err(unsafe_at(&shown, "is a hard link to another file"));
        }
        let size = ((info.nFileSizeHigh as u64) << 32) | info.nFileSizeLow as u64;
        if size > max as u64 {
            return Err(SafeError::TooLarge(shown, max));
        }
        let bytes = read_all_handle(handle.as_raw_handle(), max + 1).map_err(SafeError::from)?;
        drop(handle);
        if bytes.len() > max {
            return Err(SafeError::TooLarge(shown, max));
        }
        Ok(bytes)
    }

    pub fn read_file(&self, name: &str, max: usize) -> Result<String> {
        let bytes = self.read_bytes(name, max)?;
        String::from_utf8(bytes).map_err(|_| SafeError::NotUtf8(self.shown(name)))
    }

    pub fn sync(&self) -> Result<()> {
        Ok(())
    }

    pub fn lock(&self) -> Result<Lock> {
        let name = format!("Local\\mirante-safedir-{:x}-{:x}", self.volume_serial, self.file_index);
        let wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
        // SAFETY: name is NUL-terminated UTF-16 for CreateMutexW.
        let handle = unsafe { CreateMutexW(null_mut(), 0, wide.as_ptr()) };
        if handle.is_null() {
            return Err(io::Error::last_os_error().into());
        }
        let wait = unsafe { WaitForSingleObject(handle, INFINITE) };
        if wait != WAIT_OBJECT_0 && wait != WAIT_ABANDONED {
            // SAFETY: mutex was created but not acquired; close it.
            unsafe { CloseHandle(handle) };
            return Err(io::Error::from_raw_os_error(unsafe { GetLastError() } as i32).into());
        }
        Ok(Lock { handle, _not_send: PhantomData })
    }

    pub fn unlink(&self, name: &str) -> Result<()> {
        let _c = cname(name)?;
        let opts = FILE_OPEN_REPARSE_POINT | FILE_OPEN_FOR_BACKUP_INTENT | FILE_READ_ATTRIBUTES;
        let handle = nt_create(self.raw(), name, DELETE | FILE_READ_ATTRIBUTES, share_all(), FILE_OPEN, opts)
            .map_err(SafeError::from)?;
        let info = by_handle(handle.as_raw_handle())?;
        if is_directory(info.dwFileAttributes) && !is_reparse(info.dwFileAttributes) {
            return Err(unsafe_at(&self.shown(name), "is not a regular file"));
        }
        delete_by_handle(handle).map_err(SafeError::from)
    }

    pub fn write_new(&self, name: &str, bytes: &[u8], mode: u32) -> Result<()> {
        let _ = mode;
        let _c = cname(name)?;
        let owned = match nt_create_status(
            self.raw(),
            name,
            FILE_WRITE_ACCESS,
            share_all(),
            FILE_CREATE,
            FILE_NON_DIRECTORY_FILE | FILE_OPEN_REPARSE_POINT | FILE_SYNCHRONOUS_IO_NONALERT,
            FILE_ATTRIBUTE_NORMAL,
        ) {
            Ok(h) => h,
            Err(STATUS_OBJECT_NAME_COLLISION) => return Err(SafeError::Exists(name.to_string())),
            Err(status) => return Err(SafeError::Io(format!("NtCreateFile 0x{:08X}", status as u32))),
        };
        write_all_handle(owned.as_raw_handle(), bytes).map_err(SafeError::from)?;
        drop(owned);
        Ok(())
    }

    pub fn write_temp(&self, name: &str, bytes: &[u8], mode: u32) -> Result<String> {
        let tmp = format!(".{name}.tmp-{}", uuid::Uuid::now_v7());
        self.write_new(&tmp, bytes, mode)?;
        Ok(tmp)
    }

    fn link_at(&self, from: &str, to: &str) -> io::Result<()> {
        let from_handle = nt_create(
            self.raw(),
            from,
            FILE_GENERIC_READ,
            share_all(),
            FILE_OPEN,
            FILE_OPEN_REPARSE_POINT | FILE_NON_DIRECTORY_FILE,
        )?;
        let wide = wide_name(to)?;
        let mut buf = name_info_buffer::<FILE_LINK_INFORMATION>(&wide)?;
        let base = buf.as_mut_ptr() as *mut FILE_LINK_INFORMATION;
        // SAFETY: `buf` holds a properly sized FILE_LINK_INFORMATION with trailing name buffer.
        unsafe {
            (*base).Anonymous.ReplaceIfExists = false;
            (*base).RootDirectory = self.raw() as HANDLE;
            (*base).FileNameLength = u32::try_from(wide.len() * 2).expect("name byte length");
            std::ptr::copy_nonoverlapping(wide.as_ptr(), (*base).FileName.as_mut_ptr(), wide.len());
        }
        let len = u32::try_from(buf.len()).expect("buffer length");
        nt_set_information(from_handle.as_raw_handle(), FileLinkInformation, buf.as_ptr(), len)?;
        Ok(())
    }

    pub fn create_file(&self, name: &str, bytes: &[u8], mode: u32) -> Result<()> {
        let _lock = self.lock()?;
        let tmp = self.write_temp(name, bytes, mode)?;
        match self.link_at(&tmp, name) {
            Ok(()) => {
                let _ = self.unlink(&tmp);
                self.sync()
            }
            Err(e) if e.kind() == ErrorKind::AlreadyExists => {
                let _ = self.unlink(&tmp);
                Err(SafeError::Exists(name.to_string()))
            }
            Err(e) => {
                let _ = self.unlink(&tmp);
                Err(e.into())
            }
        }
    }

    fn rename_internal(&self, from: &str, to: &str, flags: u32) -> io::Result<()> {
        let from_handle = nt_create(
            self.raw(),
            from,
            DELETE | FILE_GENERIC_READ | SYNCHRONIZE,
            share_all(),
            FILE_OPEN,
            FILE_OPEN_REPARSE_POINT | FILE_OPEN_FOR_BACKUP_INTENT,
        )?;
        let wide = wide_name(to)?;
        let mut buf = name_info_buffer::<FILE_RENAME_INFORMATION>(&wide)?;
        let base = buf.as_mut_ptr() as *mut FILE_RENAME_INFORMATION;
        // SAFETY: `buf` holds a properly sized FILE_RENAME_INFORMATION with trailing name storage.
        unsafe {
            (*base).Anonymous.Flags = flags;
            (*base).RootDirectory = self.raw() as HANDLE;
            (*base).FileNameLength = u32::try_from(wide.len() * 2).expect("name byte length");
            std::ptr::copy_nonoverlapping(wide.as_ptr(), (*base).FileName.as_mut_ptr(), wide.len());
        }
        let len = u32::try_from(buf.len()).expect("buffer length");
        nt_set_information(from_handle.as_raw_handle(), FileRenameInformationEx, buf.as_ptr(), len)?;
        Ok(())
    }

    pub fn rename(&self, from: &str, to: &str) -> Result<()> {
        let _ = (cname(from)?, cname(to)?);
        self.rename_internal(from, to, FILE_RENAME_REPLACE_IF_EXISTS | FILE_RENAME_POSIX_SEMANTICS)
            .map_err(SafeError::from)
    }

    pub fn rename_new(&self, from: &str, to: &str) -> Result<()> {
        let _ = (cname(from)?, cname(to)?);
        match self.rename_internal(from, to, 0) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == ErrorKind::AlreadyExists => Err(SafeError::Exists(to.to_string())),
            Err(e) => Err(e.into()),
        }
    }

    pub fn remove_tree(&self, name: &str) -> Result<()> {
        self.remove_tree_at(name, 0)
    }

    fn remove_tree_at(&self, name: &str, depth: usize) -> Result<()> {
        if depth > MAX_DEPTH {
            return Err(unsafe_at(&self.shown(name), "is nested too deep"));
        }
        let Some(dir) = self.open_dir(name)? else {
            return Ok(());
        };
        loop {
            let batch = dir.entries(256)?;
            if batch.is_empty() {
                break;
            }
            for e in batch {
                if e.is_dir() {
                    dir.remove_tree_at(&e.name, depth + 1)?;
                } else {
                    dir.unlink(&e.name)?;
                }
            }
        }
        self.remove_empty_dir(name)
    }

    fn remove_empty_dir(&self, name: &str) -> Result<()> {
        let _c = cname(name)?;
        let opts = FILE_OPEN_REPARSE_POINT | FILE_OPEN_FOR_BACKUP_INTENT | FILE_DIRECTORY_FILE;
        let handle = nt_create(self.raw(), name, DELETE | FILE_READ_ATTRIBUTES, share_all(), FILE_OPEN, opts)
            .map_err(SafeError::from)?;
        let info = by_handle(handle.as_raw_handle())?;
        if is_reparse(info.dwFileAttributes) {
            return Err(unsafe_at(&self.shown(name), "is a symlink"));
        }
        delete_by_handle(handle).map_err(SafeError::from)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TempDir;
    use std::process::Command;
    use std::sync::{Arc, Barrier};
    use std::thread;
    use std::time::{Duration, Instant};

    fn win_path(p: &Path) -> String {
        let s = p.to_string_lossy();
        if let Some(rest) = s.strip_prefix(r"\\?\") {
            rest.to_string()
        } else {
            s.into_owned()
        }
    }

    fn junction(link: &Path, target: &Path) -> io::Result<()> {
        let parent = link.parent().ok_or_else(|| io::Error::new(ErrorKind::InvalidInput, "junction has no parent"))?;
        std::fs::create_dir_all(parent)?;
        let parent = std::fs::canonicalize(parent)?;
        let link_abs = parent.join(link.file_name().ok_or_else(|| io::Error::new(ErrorKind::InvalidInput, "no leaf"))?);
        let target = std::fs::canonicalize(target)?;
        let link_s = win_path(&link_abs).replace('/', "\\");
        let target_s = win_path(&target).replace('/', "\\");
        let output = Command::new("cmd").args(["/C", "mklink", "/J", &link_s, &target_s]).output()?;
        if output.status.success() {
            Ok(())
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let stdout = String::from_utf8_lossy(&output.stdout);
            Err(io::Error::other(format!("mklink /J failed ({link_s} -> {target_s}): {stdout}{stderr}")))
        }
    }

    fn skip_or_panic(msg: &str) {
        if std::env::var_os("CI").is_some() {
            panic!("{msg}");
        }
        eprintln!("SKIPPED: {msg}");
    }

    #[test]
    fn opens_nested_levels_without_following_junctions() {
        let d = TempDir::new("safedir");
        std::fs::create_dir_all(d.path().join("a/b/c")).unwrap();
        let c = SafeDir::open(d.path(), &["a", "b", "c"]).unwrap().unwrap();
        assert!(c.path().to_string_lossy().ends_with("a\\b\\c") || c.path().to_string_lossy().ends_with("a/b/c"));
        assert!(SafeDir::open(d.path(), &["a", "missing"]).unwrap().is_none());
        let outside = TempDir::new("safedir-out");
        std::fs::create_dir(outside.path().join("c")).unwrap();
        std::fs::remove_dir(d.path().join("a/b/c")).unwrap();
        std::fs::rename(d.path().join("a/b"), d.path().join("a/kept")).unwrap();
        junction(&d.path().join("a/b"), outside.path()).expect("junction");
        assert!(matches!(SafeDir::open(d.path(), &["a", "b", "c"]), Err(SafeError::Unsafe(_))));
        assert!(matches!(SafeDir::create(d.path(), &["a", "b", "x"], 0o700), Err(SafeError::Unsafe(_))));
        assert!(!outside.path().join("x").exists());
        let a = SafeDir::open(d.path(), &["a"]).unwrap().unwrap();
        assert!(matches!(a.open_dir("b"), Err(SafeError::Unsafe(_))));
        assert!(a.open_dir("kept").unwrap().is_some());
        let n = SafeDir::create(d.path(), &["n1", "n2"], 0o700).unwrap();
        assert!(n.path().to_string_lossy().contains("n1"));
    }

    #[test]
    fn create_dir_new_refuses_existing_and_links() {
        let d = TempDir::new("safedir-new");
        let root = SafeDir::open(d.path(), &[]).unwrap().unwrap();
        root.create_dir_new("x", 0o700).unwrap();
        assert_eq!(root.create_dir_new("x", 0o700).unwrap_err(), SafeError::Exists("x".into()));
        let outside = TempDir::new("outside");
        junction(&d.path().join("y"), outside.path()).expect("junction");
        assert_eq!(root.create_dir_new("y", 0o700).unwrap_err(), SafeError::Exists("y".into()));
        std::fs::write(d.path().join("f"), "x").unwrap();
        assert!(root.create_dir_new("f", 0o700).is_err());
        assert!(root.create_dir_new("../z", 0o700).is_err());
    }

    #[test]
    fn rename_new_never_replaces_even_an_empty_dir() {
        let d = TempDir::new("safedir-rename");
        let root = SafeDir::open(d.path(), &[]).unwrap().unwrap();
        let s1 = root.create_dir_new(".stage-1", 0o700).unwrap();
        s1.write_new("a.txt", b"one", 0o600).unwrap();
        root.rename_new(".stage-1", "v1").unwrap();
        assert_eq!(std::fs::read_to_string(d.path().join("v1/a.txt")).unwrap(), "one");
        root.create_dir_new(".stage-2", 0o700).unwrap();
        assert_eq!(root.rename_new(".stage-2", "v1").unwrap_err(), SafeError::Exists("v1".into()));
        root.create_dir_new("v2", 0o700).unwrap();
        assert_eq!(root.rename_new(".stage-2", "v2").unwrap_err(), SafeError::Exists("v2".into()));
        assert!(d.path().join(".stage-2").is_dir());
    }

    #[test]
    fn rename_replaces_a_file() {
        let d = TempDir::new("safedir-rename-file");
        let root = SafeDir::open(d.path(), &[]).unwrap().unwrap();
        root.write_new("a", b"old", 0o600).unwrap();
        root.write_new("b", b"new", 0o600).unwrap();
        root.rename("b", "a").unwrap();
        assert_eq!(root.read_file("a", 10).unwrap(), "new");
    }

    #[test]
    fn write_new_and_create_file_never_clobber() {
        let d = TempDir::new("safedir-write");
        let root = SafeDir::open(d.path(), &[]).unwrap().unwrap();
        root.write_new("a", b"1", 0o600).unwrap();
        assert_eq!(root.write_new("a", b"2", 0o600).unwrap_err(), SafeError::Exists("a".into()));
        let missing = d.path().join("missing-target");
        std::fs::create_dir(&missing).unwrap();
        junction(&d.path().join("dangling"), &missing).expect("junction");
        std::fs::remove_dir(&missing).unwrap();
        assert!(root.write_new("dangling", b"x", 0o600).is_err());
        assert!(root.create_file("dangling", b"x", 0o600).is_err());
        root.create_file("b", b"two", 0o600).unwrap();
        assert_eq!(root.read_file("b", 10).unwrap(), "two");
        assert!(matches!(root.read_file("b", 2), Err(SafeError::TooLarge(_, 2))));
    }

    #[test]
    fn remove_tree_unlinks_junctions_and_never_follows_them() {
        let d = TempDir::new("safedir-rm");
        let outside = TempDir::new("safedir-rm-out");
        std::fs::write(outside.path().join("keep"), "keep").unwrap();
        std::fs::create_dir_all(d.path().join("t/sub/deeper")).unwrap();
        std::fs::write(d.path().join("t/sub/deeper/f"), "x").unwrap();
        std::fs::write(d.path().join("t/g"), "x").unwrap();
        junction(&d.path().join("t/link"), outside.path()).expect("junction");
        let root = SafeDir::open(d.path(), &[]).unwrap().unwrap();
        root.remove_tree("t").unwrap();
        assert!(!d.path().join("t").exists());
        assert_eq!(std::fs::read_to_string(outside.path().join("keep")).unwrap(), "keep");
        root.remove_tree("t").unwrap();
    }

    #[test]
    fn read_refuses_hard_links() {
        let d = TempDir::new("safedir-hlink");
        let root = SafeDir::open(d.path(), &[]).unwrap().unwrap();
        std::fs::write(d.path().join("f"), b"secret").unwrap();
        std::fs::hard_link(d.path().join("f"), d.path().join("h")).unwrap();
        assert!(matches!(root.read_bytes("f", 100), Err(SafeError::Unsafe(_))));
        assert!(matches!(root.read_bytes("h", 100), Err(SafeError::Unsafe(_))));
    }

    #[test]
    fn read_refuses_links_without_reading_the_target() {
        let d = TempDir::new("safedir-read-link");
        let outside = TempDir::new("outside-read");
        std::fs::write(outside.path().join("secret"), b"outside-bytes").unwrap();
        junction(&d.path().join("j"), outside.path()).expect("junction");
        let root = SafeDir::open(d.path(), &[]).unwrap().unwrap();
        assert!(matches!(root.read_bytes("j", 1000), Err(SafeError::Unsafe(_))));
        assert!(matches!(root.read_file("j", 1000), Err(SafeError::Unsafe(_))));
        assert!(matches!(root.stat("j"), Err(SafeError::Unsafe(_))));
        let file_outside = outside.path().join("secret");
        if let Err(e) = std::os::windows::fs::symlink_file(&file_outside, d.path().join("sym")) {
            skip_or_panic(&format!("symlink_file unavailable: {e}"));
            return;
        }
        assert!(matches!(root.read_bytes("sym", 1000), Err(SafeError::Unsafe(_))));
        assert_ne!(root.read_bytes("sym", 1000), Ok(b"outside-bytes".to_vec()));
    }

    #[test]
    fn unlink_and_remove_tree_never_touch_link_targets() {
        let d = TempDir::new("safedir-unlink");
        let empty = TempDir::new("empty-dir");
        junction(&d.path().join("jempty"), empty.path()).expect("junction");
        let full = TempDir::new("full-dir");
        std::fs::write(full.path().join("inside"), b"stay").unwrap();
        junction(&d.path().join("jfull"), full.path()).expect("junction");
        let root = SafeDir::open(d.path(), &[]).unwrap().unwrap();
        root.unlink("jempty").unwrap();
        assert!(empty.path().is_dir());
        root.unlink("jfull").unwrap();
        assert_eq!(std::fs::read_to_string(full.path().join("inside")).unwrap(), "stay");
        let outside_file = TempDir::new("file-out");
        std::fs::write(outside_file.path().join("f"), b"file-stay").unwrap();
        if std::os::windows::fs::symlink_file(outside_file.path().join("f"), d.path().join("symf")).is_err() {
            skip_or_panic("symlink_file unavailable");
            return;
        }
        root.unlink("symf").unwrap();
        assert_eq!(std::fs::read_to_string(outside_file.path().join("f")).unwrap(), "file-stay");
    }

    #[test]
    fn entries_and_stat_report_kinds() {
        let d = TempDir::new("safedir-entries");
        let root = SafeDir::open(d.path(), &[]).unwrap().unwrap();
        root.write_new("f", b"x", 0o600).unwrap();
        root.create_dir_new("sub", 0o700).unwrap();
        let outside = TempDir::new("ent-out");
        junction(&d.path().join("j"), outside.path()).expect("junction");
        let entries = root.entries(100).unwrap();
        let f = entries.iter().find(|e| e.name == "f").expect("file");
        assert!(f.is_file());
        let sub = entries.iter().find(|e| e.name == "sub").expect("dir");
        assert!(sub.is_dir());
        let j = entries.iter().find(|e| e.name == "j").expect("junction");
        assert!(!j.is_file() && !j.is_dir());
        let st = root.stat("f").unwrap().unwrap();
        assert!(st.is_file());
    }

    #[test]
    fn lock_is_exclusive_across_threads() {
        let d = TempDir::new("safedir-lock");
        let root = Arc::new(SafeDir::open(d.path(), &[]).unwrap().unwrap());
        let barrier = Arc::new(Barrier::new(2));
        let root2 = Arc::clone(&root);
        let barrier2 = Arc::clone(&barrier);
        let t = thread::spawn(move || {
            let _lock = root2.lock().unwrap();
            barrier2.wait();
            thread::sleep(Duration::from_millis(400));
        });
        barrier.wait();
        let start = Instant::now();
        let _lock = root.lock().unwrap();
        assert!(start.elapsed() >= Duration::from_millis(200));
        t.join().unwrap();
    }

    #[test]
    fn lock_helper_holds_the_lock() {
        if std::env::var_os("MIRANTE_TEST_LOCK_HELPER").is_none() {
            return;
        }
        let path = std::env::var("MIRANTE_TEST_LOCK_DIR").expect("MIRANTE_TEST_LOCK_DIR");
        let root = SafeDir::open(Path::new(&path), &[]).unwrap().unwrap();
        let _lock = root.lock().unwrap();
        println!("LOCKED");
        thread::sleep(Duration::from_secs(2));
    }

    #[test]
    fn lock_is_exclusive_across_processes() {
        if std::env::var_os("MIRANTE_TEST_LOCK_HELPER").is_some() {
            return;
        }
        let d = TempDir::new("safedir-lock-xp");
        let root = SafeDir::open(d.path(), &[]).unwrap().unwrap();
        let exe = std::env::current_exe().unwrap();
        let mut child = Command::new(&exe)
            .env("MIRANTE_TEST_LOCK_HELPER", "1")
            .env("MIRANTE_TEST_LOCK_DIR", d.path())
            .args(["safedir::windows::tests::lock_helper_holds_the_lock", "--exact", "--nocapture", "--test-threads=1"])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("spawn helper");
        let mut stdout = String::new();
        use std::io::BufRead;
        let deadline = Instant::now() + Duration::from_secs(10);
        if let Some(mut out) = child.stdout.take() {
            let mut reader = std::io::BufReader::new(&mut out);
            while Instant::now() < deadline {
                let mut line = String::new();
                match reader.read_line(&mut line) {
                    Ok(0) => break,
                    Ok(_) => {
                        stdout.push_str(&line);
                        if line.contains("LOCKED") {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        }
        assert!(stdout.contains("LOCKED"), "stdout: {stdout}");
        let start = Instant::now();
        let _lock = root.lock().unwrap();
        assert!(start.elapsed() >= Duration::from_millis(500));
        let _ = child.wait();
    }

    #[test]
    fn cname_refuses_windows_separators() {
        for bad in ["a\\b", "a:b", "a.", "a ", "CON", "nul.txt"] {
            assert!(cname(bad).is_err(), "{bad}");
        }
        let long = "a".repeat(256);
        assert!(cname(&long).is_err());
    }
}
