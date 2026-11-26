
use std::borrow::Cow;
use std::ffi::{OsStr, OsString};
use std::fs::{self, File, Metadata, OpenOptions};
use std::io;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
use std::os::windows::io::{AsRawHandle, FromRawHandle};
use std::path::{Component, Path, PathBuf, Prefix};
use std::sync::OnceLock;
use std::time::UNIX_EPOCH;

use windows::core::{HSTRING, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, GetSecurityInfo,
    SDDL_REVISION_1, SE_FILE_OBJECT,
};
use windows::Win32::Security::{OWNER_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID, SECURITY_ATTRIBUTES};
use windows::Win32::Storage::FileSystem::*;
use windows::Win32::System::IO::OVERLAPPED;

pub fn name_bytes(name: &OsStr) -> Cow<'_, [u8]> {
    if let Some(text) = name.to_str() {
        Cow::Borrowed(text.as_bytes())
    } else {
        Cow::Owned(std::iter::once(255).chain(name.encode_wide().flat_map(u16::to_le_bytes)).collect())
    }
}

pub fn name_from_bytes(bytes: Vec<u8>) -> Option<OsString> {
    if let Some(wide) = bytes.strip_prefix(&[255]) {
        if wide.len() % 2 != 0 {
            return None;
        }
        let name = OsString::from_wide(&wide.chunks_exact(2)
            .map(|unit| u16::from_le_bytes([unit[0], unit[1]])).collect::<Vec<_>>());
        return name.to_str().is_none().then_some(name);
    }
    String::from_utf8(bytes).ok().map(OsString::from)
}

pub fn valid_component(name: &OsStr) -> bool {
    !name.is_empty() && name != "." && name != ".."
        && !name.encode_wide().any(|unit| matches!(unit, 0..=31 | 34 | 42 | 47 | 58 | 60 | 62 | 63 | 92 | 124))
}

pub fn canonical_name(name: &OsStr) -> Cow<'_, OsStr> {
    Cow::Owned(name.to_ascii_lowercase())
}

pub fn entry_name(path: &Path) -> io::Result<OsString> {
    let mut found = WIN32_FIND_DATAW::default();
    let path = HSTRING::from(path.as_os_str());
    unsafe {
        // FindFirstFile returns the long name even when the requested entry is its 8.3 alias.
        let find = FindFirstFileW(&path, &mut found).map_err(error)?;
        let _ = FindClose(find);
    }
    let end = found.cFileName.iter().position(|&unit| unit == 0).unwrap_or(found.cFileName.len());
    Ok(OsString::from_wide(&found.cFileName[..end]))
}

pub fn valid_name(name: &str) -> bool {
    if !valid_component(OsStr::new(name)) || name.ends_with(['.', ' '])
        || name.encode_utf16().count() > 255
    {
        return false;
    }
    let stem = name.split('.').next().unwrap_or_default().trim_end_matches(' ').to_ascii_uppercase();
    if matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$") {
        return false;
    }
    !stem.strip_prefix("COM").or_else(|| stem.strip_prefix("LPT"))
        .is_some_and(|number| matches!(number, "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"))
}

fn drive(name: &OsStr) -> Option<u8> {
    match name.to_str()?.as_bytes() {
        [letter, b':'] if letter.is_ascii_alphabetic() => Some(letter.to_ascii_uppercase()),
        _ => None,
    }
}

pub fn directory(parts: &[OsString]) -> Option<PathBuf> {
    let Some((root, parts)) = parts.split_first() else { return Some(PathBuf::new()); };
    let mut path = if let Some(letter) = drive(root) {
        PathBuf::from(format!("\\\\?\\{}:\\", char::from(letter)))
    } else {
        let root = root.to_str()?.strip_prefix("\\\\")?;
        let (host, share) = root.split_once('\\')?;
        if !valid_name(host) || !valid_name(share) {
            return None;
        }
        PathBuf::from(format!("\\\\?\\UNC\\{host}\\{share}\\"))
    };
    for part in parts {
        if !valid_component(part) { return None; }
        path.push(part);
    }
    Some(path)
}

pub fn path_parts(path: &Path) -> Vec<OsString> {
    path.components().filter_map(|part| match part {
        Component::Prefix(prefix) => match prefix.kind() {
            Prefix::Disk(letter) | Prefix::VerbatimDisk(letter) => Some(format!("{}:", char::from(letter)).into()),
            Prefix::UNC(host, share) | Prefix::VerbatimUNC(host, share) => {
                let mut root = OsString::from("\\\\");
                root.push(host);
                root.push("\\");
                root.push(share);
                Some(root)
            }
            _ => None,
        },
        Component::Normal(name) => Some(name.to_os_string()),
        _ => None,
    }).collect()
}

pub fn roots() -> io::Result<Vec<OsString>> {
    let mask = unsafe { GetLogicalDrives() };
    if mask == 0 { return Err(io::Error::last_os_error()); }
    Ok((0..26).filter(|i| mask & (1 << i) != 0)
        .map(|i| OsString::from(format!("{}:", char::from(b'A' + i)))).collect())
}

pub fn read_names(path: &Path, mut visit: impl FnMut(&OsStr, bool)) -> io::Result<()> {
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let meta = entry.metadata()?;
        visit(&entry.file_name(), meta.is_dir() && !is_link(&meta));
    }
    Ok(())
}

pub type Owners = ();
pub fn mode(_: &Metadata) -> Option<u32> { None }
pub fn owner_for(_: &Metadata, _: &mut Owners) -> Option<String> { None }

pub fn attributes(meta: &Metadata) -> Option<Vec<String>> {
    Some([
        (FILE_ATTRIBUTE_READONLY, "read_only"),
        (FILE_ATTRIBUTE_HIDDEN, "hidden"),
        (FILE_ATTRIBUTE_SYSTEM, "system"),
    ].into_iter().filter(|(attribute, _)| meta.file_attributes() & attribute.0 != 0)
        .map(|(_, label)| label.into()).collect())
}

pub fn modified(meta: &Metadata) -> i64 {
    meta.modified().map_or(0, |time| match time.duration_since(UNIX_EPOCH) {
        Ok(after) => after.as_secs() as i64,
        Err(before) => -(before.duration().as_secs() as i64),
    })
}

pub fn is_link(meta: &Metadata) -> bool {
    // On Windows Rust recognizes name-surrogate reparse points, including junctions.
    meta.is_symlink()
}

pub fn is_regular(meta: &Metadata) -> bool {
    meta.is_file()
}

pub fn remove_entry(path: &Path) -> io::Result<()> {
    let meta = fs::symlink_metadata(path)?;
    if meta.file_attributes() & FILE_ATTRIBUTE_DIRECTORY.0 != 0 {
        fs::remove_dir(path)
    } else {
        fs::remove_file(path)
    }
}

fn handle(file: &File) -> HANDLE {
    HANDLE(file.as_raw_handle())
}

fn error(err: windows::core::Error) -> io::Error {
    let code = err.code().0 as u32;
    if code >> 16 == 0x8007 { io::Error::from_raw_os_error((code & 0xffff) as i32) }
    else { io::Error::other(err) }
}

pub fn pin(path: &Path) -> io::Result<File> {
    OpenOptions::new().access_mode(FILE_READ_ATTRIBUTES.0)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT.0 | FILE_FLAG_BACKUP_SEMANTICS.0)
        .open(path)
}

pub fn pin_directory(path: &Path) -> io::Result<File> {
    let folder = OpenOptions::new().access_mode(FILE_READ_ATTRIBUTES.0)
        .share_mode((FILE_SHARE_READ | FILE_SHARE_WRITE).0)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT.0 | FILE_FLAG_BACKUP_SEMANTICS.0)
        .open(path)?;
    let meta = folder.metadata()?;
    if !meta.is_dir() || is_link(&meta) {
        return Err(io::ErrorKind::NotADirectory.into());
    }
    Ok(folder)
}

pub fn pinned_path(file: &File) -> io::Result<PathBuf> {
    let mut wide = vec![0; 512];
    loop {
        let len = unsafe { GetFinalPathNameByHandleW(handle(file), &mut wide, FILE_NAME_NORMALIZED) } as usize;
        if len == 0 { return Err(io::Error::last_os_error()); }
        if len < wide.len() {
            return Ok(PathBuf::from(OsString::from_wide(&wide[..len])));
        }
        wide.resize(len + 1, 0);
    }
}

pub fn open_pinned(file: &File) -> io::Result<File> {
    reopen(file, FILE_GENERIC_READ)
}

fn reopen(file: &File, access: FILE_ACCESS_RIGHTS) -> io::Result<File> {
    let reopened = unsafe {
        ReOpenFile(handle(file), access.0, FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE, FILE_FLAG_OPEN_REPARSE_POINT)
    }.map_err(error)?;
    Ok(unsafe { File::from_raw_handle(reopened.0) })
}

fn user_sid() -> Option<&'static String> {
    static USER: OnceLock<Option<String>> = OnceLock::new();
    USER.get_or_init(|| crate::service::ipc::own_sid().ok()).as_ref()
}

pub fn create_partial(path: &Path) -> io::Result<File> {
    let sid = user_sid().ok_or(io::ErrorKind::PermissionDenied)?;
    let mut descriptor = PSECURITY_DESCRIPTOR::default();
    let path = HSTRING::from(path.as_os_str());
    unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(&HSTRING::from(format!("D:P(A;;FA;;;{sid})")),
            SDDL_REVISION_1, &mut descriptor, None).map_err(error)?;
        let security = SECURITY_ATTRIBUTES {
            nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: descriptor.0, bInheritHandle: false.into(),
        };
        let result = CreateFileW(&path, (FILE_GENERIC_READ | FILE_GENERIC_WRITE | DELETE).0,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE, Some(&security),
            CREATE_NEW, FILE_ATTRIBUTE_NORMAL, None).map_err(error);
        let _ = LocalFree(Some(HLOCAL(descriptor.0)));
        Ok(File::from_raw_handle(result?.0))
    }
}

pub fn discard_partial(file: &File, _: &File, _: &OsStr) {
    let disposition = FILE_DISPOSITION_INFO_EX {
        Flags: FILE_DISPOSITION_INFO_EX_FLAGS(FILE_DISPOSITION_FLAG_DELETE.0 | FILE_DISPOSITION_FLAG_POSIX_SEMANTICS.0 | FILE_DISPOSITION_FLAG_IGNORE_READONLY_ATTRIBUTE.0),
    };
    unsafe {
        if SetFileInformationByHandle(handle(file), FileDispositionInfoEx,
            (&disposition as *const FILE_DISPOSITION_INFO_EX).cast(), size_of::<FILE_DISPOSITION_INFO_EX>() as u32).is_err() {
            // A volume without POSIX unlink can still delete through the existing handle.
            let disposition = FILE_DISPOSITION_INFO { DeleteFile: true };
            let _ = SetFileInformationByHandle(handle(file), FileDispositionInfo,
                (&disposition as *const FILE_DISPOSITION_INFO).cast(), size_of::<FILE_DISPOSITION_INFO>() as u32);
        }
    }
}

pub fn lock_exclusive(file: &File) -> io::Result<()> {
    let mut position = OVERLAPPED::default();
    // A byte past any transfer data acts as the advisory lock.
    position.Anonymous.Anonymous.OffsetHigh = 0x7fff_ffff;
    unsafe { LockFileEx(handle(file), LOCKFILE_EXCLUSIVE_LOCK | LOCKFILE_FAIL_IMMEDIATELY, None, 1, 0, &mut position) }
        .map_err(|err| {
            let err = error(err);
            if err.raw_os_error() == Some(ERROR_LOCK_VIOLATION.0 as i32) {
                io::ErrorKind::WouldBlock.into()
            } else { err }
        })
}

pub type Identity = (u32, u32, u32);

pub fn identity(file: &File) -> io::Result<Identity> {
    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    unsafe { GetFileInformationByHandle(handle(file), &mut info) }.map_err(error)?;
    Ok((info.dwVolumeSerialNumber, info.nFileIndexHigh, info.nFileIndexLow))
}

pub fn owned(file: &File, _: &Metadata) -> bool {
    let Some(user) = user_sid() else { return false; };
    let Ok(file) = reopen(file, READ_CONTROL) else { return false; };
    unsafe {
        let mut owner = PSID::default();
        let mut descriptor = PSECURITY_DESCRIPTOR::default();
        if GetSecurityInfo(handle(&file), SE_FILE_OBJECT, OWNER_SECURITY_INFORMATION,
            Some(&mut owner), None, None, None, Some(&mut descriptor)).is_err() {
            return false;
        }
        let mut text = windows::core::PWSTR::null();
        let matches = ConvertSidToStringSidW(owner, &mut text).is_ok()
            && text.to_string().is_ok_and(|sid| &sid == user);
        let _ = LocalFree(Some(HLOCAL(text.0.cast())));
        let _ = LocalFree(Some(HLOCAL(descriptor.0.cast())));
        matches
    }
}

fn rename(from: &Path, to: &Path, flags: MOVE_FILE_FLAGS) -> io::Result<()> {
    let wide = |path: &Path| -> io::Result<HSTRING> {
        let path = std::path::absolute(path)?;
        if path.as_os_str().as_encoded_bytes().starts_with(b"\\\\?\\") {
            return Ok(HSTRING::from(path.as_os_str()));
        }
        let mut extended = OsString::from("\\\\?\\");
        if let Ok(unc) = path.strip_prefix("\\\\") {
            extended.push("UNC\\");
            extended.push(unc);
        } else {
            extended.push(path);
        }
        Ok(HSTRING::from(extended.as_os_str()))
    };
    let (from, to) = (wide(from)?, wide(to)?);
    unsafe { MoveFileExW(PCWSTR(from.as_ptr()), PCWSTR(to.as_ptr()), flags) }.map_err(error)
}

pub fn rename_new(from: &Path, to: &Path) -> io::Result<()> {
    rename(from, to, MOVE_FILE_FLAGS(0))
}

pub fn replace(from: &Path, to: &Path) -> io::Result<()> {
    rename(from, to, MOVEFILE_REPLACE_EXISTING)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_names_are_single_windows_components() {
        for (name, valid) in [
            ("", false), (".", false), ("..", false), ("data.txt", true), (".hidden", true),
            ("a/b", false), ("a\\b", false), ("a:b", false), ("a*b", false), ("a\u{1f}", false),
            ("trailing.", false), ("trailing ", false), ("NUL.txt", false), ("con .txt", false),
            ("CoM¹", false), ("LPT9.log", false), ("COM10", true), ("auxiliary", true),
        ] {
            assert_eq!(valid_name(name), valid, "{name:?}");
        }
        assert!(!valid_name(&"😀".repeat(128)));
    }

    #[test]
    fn roots_round_trip_as_windows_paths() {
        assert!(directory(&[]).unwrap().as_os_str().is_empty());
        let drives = roots().unwrap();
        assert!(!drives.is_empty());
        for root in drives {
            let parts = vec![root, "Users".into()];
            assert_eq!(path_parts(&directory(&parts).unwrap()), parts);
        }
        assert!(directory(&["C:".into(), "..".into()]).is_none());
    }


    #[test]
    fn a_long_path_renames_without_replacing_and_publishes_in_one_move() {
        let dir = folder();
        let long = dir.join("a".repeat(110)).join("b".repeat(110)).join("c".repeat(110));
        fs::create_dir_all(&long).unwrap();
        let (from, to) = (long.join("from"), long.join("to"));
        fs::write(&from, b"new").unwrap();
        fs::write(&to, b"original").unwrap();
        assert_eq!(rename_new(&from, &to).unwrap_err().kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read(&to).unwrap(), b"original");
        replace(&from, &to).unwrap();
        assert_eq!(fs::read(&to).unwrap(), b"new");
        assert!(!from.exists());
        rename_new(&to, &from).unwrap();
        assert_eq!(fs::read(&from).unwrap(), b"new");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn directory_reads_include_hidden_system_and_read_only_attributes() {
        let dir = folder();
        let path = dir.join("hidden");
        fs::write(&path, []).unwrap();
        let wide = HSTRING::from(path.as_os_str());
        let flags = FILE_ATTRIBUTE_READONLY | FILE_ATTRIBUTE_HIDDEN | FILE_ATTRIBUTE_SYSTEM;
        unsafe { SetFileAttributesW(&wide, flags) }.unwrap();
        let mut listed = false;
        read_names(&dir, |name, folder| listed |= name == "hidden" && !folder).unwrap();
        assert!(listed);
        assert_eq!(attributes(&path.symlink_metadata().unwrap()).unwrap(), ["read_only", "hidden", "system"]);
        unsafe { SetFileAttributesW(&wide, FILE_ATTRIBUTE_NORMAL) }.unwrap();
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn deleting_a_junction_or_symbolic_link_preserves_its_target() {
        use std::os::windows::process::CommandExt;
        let dir = folder();
        let target = dir.join("target");
        fs::create_dir(&target).unwrap();
        fs::write(target.join("keep"), b"kept").unwrap();
        let junction = dir.join("junction");
        let create = std::process::Command::new("cmd.exe")
            .args(["/D", "/C"])
            .raw_arg(r#"mklink /J "%DESKWAY_TEST_JUNCTION%" "%DESKWAY_TEST_TARGET%""#)
            .env("DESKWAY_TEST_JUNCTION", &junction)
            .env("DESKWAY_TEST_TARGET", &target)
            .output().unwrap();
        assert!(create.status.success(), "{}", String::from_utf8_lossy(&create.stderr));
        let link = dir.join("link");
        let mut links = vec![junction];
        match std::os::windows::fs::symlink_dir(&target, &link) {
            Ok(()) => links.push(link),
            Err(e) if e.raw_os_error() == Some(ERROR_PRIVILEGE_NOT_HELD.0 as i32) => {}
            Err(e) => panic!("create the symbolic link: {e}"),
        }
        for link in links {
            assert!(is_link(&link.symlink_metadata().unwrap()));
            assert_eq!(fs::read(link.join("keep")).unwrap(), b"kept");
            remove_entry(&link).unwrap();
            assert!(!link.exists());
            assert_eq!(fs::read(target.join("keep")).unwrap(), b"kept");
        }
        fs::remove_dir_all(dir).unwrap();
    }

    fn folder() -> PathBuf {
        let id = wire::crypto::hex(&wire::crypto::random::<8>());
        let path = std::env::temp_dir().join(format!("deskway-win-files-{id}"));
        fs::create_dir(&path).unwrap();
        path
    }
}
