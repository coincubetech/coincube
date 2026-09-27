//! Private Windows journal storage. Paths are pinned with no-delete-sharing
//! handles; reparse points and non-NTFS volumes are deliberately refused.
use ::windows::{
    core::{PCWSTR, PWSTR},
    Win32::{
        Foundation::{LocalFree, HANDLE, HLOCAL},
        Security::{
            Authorization::{
                ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
                GetSecurityInfo, SE_FILE_OBJECT,
            },
            *,
        },
        Storage::FileSystem::*,
        System::Threading::{GetCurrentProcess, OpenProcessToken},
    },
};
use std::{
    fs::File,
    io,
    mem::size_of,
    os::windows::{
        ffi::OsStrExt,
        io::{AsRawHandle, FromRawHandle, OwnedHandle},
    },
    path::{Component, Path, PathBuf, Prefix},
    ptr,
};
fn invalid() -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        "Claim journal requires private local NTFS storage without links",
    )
}
fn win(error: ::windows::core::Error) -> io::Error {
    io::Error::from_raw_os_error(error.code().0 & 0xffff)
}
fn wide(path: &Path) -> io::Result<Vec<u16>> {
    let mut value: Vec<_> = path.as_os_str().encode_wide().collect();
    if value.contains(&0) {
        return Err(invalid());
    }
    value.push(0);
    Ok(value)
}
fn handle(file: &File) -> HANDLE {
    HANDLE(file.as_raw_handle())
}
struct Allocation(*mut std::ffi::c_void);
impl Drop for Allocation {
    fn drop(&mut self) {
        unsafe {
            let _ = LocalFree(Some(HLOCAL(self.0)));
        }
    }
}
struct User {
    _token: OwnedHandle,
    bytes: Vec<usize>,
}
impl User {
    fn current() -> io::Result<Self> {
        unsafe {
            let mut token = HANDLE::default();
            OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).map_err(win)?;
            let token = OwnedHandle::from_raw_handle(token.0);
            let mut length = 0;
            let _ = GetTokenInformation(
                HANDLE(token.as_raw_handle()),
                TokenUser,
                None,
                0,
                &mut length,
            );
            if length == 0 || length > 65536 {
                return Err(invalid());
            }
            let mut bytes = vec![0usize; (length as usize).div_ceil(size_of::<usize>())];
            GetTokenInformation(
                HANDLE(token.as_raw_handle()),
                TokenUser,
                Some(bytes.as_mut_ptr().cast()),
                length,
                &mut length,
            )
            .map_err(win)?;
            Ok(Self {
                _token: token,
                bytes,
            })
        }
    }
    fn sid(&self) -> PSID {
        unsafe { (*(self.bytes.as_ptr().cast::<TOKEN_USER>())).User.Sid }
    }
    fn security(&self, directory: bool) -> io::Result<Allocation> {
        unsafe {
            let mut text = PWSTR::null();
            ConvertSidToStringSidW(self.sid(), &mut text).map_err(win)?;
            let _text = Allocation(text.0.cast());
            let sid = text.to_string().map_err(|_| invalid())?;
            let inheritance = if directory { "OICI" } else { "" };
            let sddl = wide(Path::new(&format!(
                "O:{sid}D:P(A;{inheritance};FA;;;{sid})"
            )))?;
            let mut descriptor = PSECURITY_DESCRIPTOR::default();
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                PCWSTR(sddl.as_ptr()),
                1,
                &mut descriptor,
                None,
            )
            .map_err(win)?;
            Ok(Allocation(descriptor.0))
        }
    }
}
fn attributes(security: &Allocation) -> SECURITY_ATTRIBUTES {
    SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: security.0,
        bInheritHandle: false.into(),
    }
}
fn validate(file: &File, directory: bool, private: bool) -> io::Result<()> {
    unsafe {
        let mut info = BY_HANDLE_FILE_INFORMATION::default();
        GetFileInformationByHandle(handle(file), &mut info).map_err(win)?;
        if GetFileType(handle(file)) != FILE_TYPE_DISK
            || info.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0
            || (info.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY.0 != 0) != directory
            || (!directory && info.nNumberOfLinks != 1)
        {
            return Err(invalid());
        }
        if !private {
            return Ok(());
        }
        let user = User::current()?;
        let mut owner = PSID::default();
        let mut acl = ptr::null_mut();
        let mut descriptor = PSECURITY_DESCRIPTOR::default();
        GetSecurityInfo(
            handle(file),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            Some(&mut owner),
            None,
            Some(&mut acl),
            None,
            Some(&mut descriptor),
        )
        .ok()
        .map_err(win)?;
        let _descriptor = Allocation(descriptor.0);
        EqualSid(owner, user.sid()).map_err(|_| invalid())?;
        let mut control = 0;
        let mut revision = 0;
        GetSecurityDescriptorControl(descriptor, &mut control, &mut revision).map_err(win)?;
        if control & SE_DACL_PROTECTED.0 == 0
            || acl.is_null()
            || !IsValidAcl(acl).as_bool()
            || (*acl).AceCount != 1
        {
            return Err(invalid());
        }
        let mut ace = ptr::null_mut();
        GetAce(acl, 0, &mut ace).map_err(win)?;
        let header = &*ace.cast::<ACE_HEADER>();
        // ACCESS_ALLOWED_ACE_TYPE = 0. Accept only the explicit owner grant.
        if header.AceType != 0
            || header.AceFlags != if directory { 3 } else { 0 }
            || usize::from(header.AceSize) < size_of::<ACCESS_ALLOWED_ACE>()
        {
            return Err(invalid());
        }
        let allowed = &*ace.cast::<ACCESS_ALLOWED_ACE>();
        let sid = PSID(ptr::addr_of!(allowed.SidStart).cast_mut().cast());
        if allowed.Mask != FILE_ALL_ACCESS.0 || !IsValidSid(sid).as_bool() {
            return Err(invalid());
        }
        EqualSid(sid, user.sid()).map_err(|_| invalid())?;
        Ok(())
    }
}
fn open(
    path: &Path,
    directory: bool,
    disposition: FILE_CREATION_DISPOSITION,
    private: bool,
) -> io::Result<File> {
    let name = wide(path)?;
    let user = User::current()?;
    let security = user.security(directory)?;
    let attrs = attributes(&security);
    let flags = FILE_FLAG_OPEN_REPARSE_POINT
        | if directory {
            FILE_FLAG_BACKUP_SEMANTICS
        } else {
            FILE_FLAGS_AND_ATTRIBUTES(0)
        };
    let access = if directory {
        READ_CONTROL.0 | FILE_READ_ATTRIBUTES.0
    } else {
        FILE_GENERIC_READ.0 | FILE_GENERIC_WRITE.0
    };
    let raw = unsafe {
        CreateFileW(
            PCWSTR(name.as_ptr()),
            access,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            Some(&attrs),
            disposition,
            flags,
            None,
        )
        .map_err(win)?
    };
    let file = unsafe { File::from_raw_handle(raw.0) };
    validate(&file, directory, private)?;
    Ok(file)
}

/// Pin each path component: OPEN_REPARSE_POINT alone protects only the leaf.
pub(super) struct Directory {
    pub(super) path: PathBuf,
    _pins: Vec<File>,
}
impl Directory {
    pub(super) fn open(path: &Path, create: bool) -> io::Result<Self> {
        let path = std::path::absolute(path)?;
        match path.components().next() {
            Some(Component::Prefix(prefix))
                if matches!(prefix.kind(), Prefix::Disk(_) | Prefix::VerbatimDisk(_)) => {}
            _ => return Err(invalid()),
        }
        let mut pins = Vec::new();
        let mut current = PathBuf::new();
        let total = path.components().count();
        for (i, component) in path.components().enumerate() {
            if !matches!(
                component,
                Component::Prefix(_) | Component::RootDir | Component::Normal(_)
            ) {
                return Err(invalid());
            }
            current.push(component);
            if matches!(component, Component::Prefix(_)) {
                continue;
            }
            let leaf = i + 1 == total;
            if matches!(component, Component::RootDir) {
                let root = wide(&current)?;
                // DRIVE_FIXED = 3; refuse remote mapped and removable storage.
                if unsafe { GetDriveTypeW(PCWSTR(root.as_ptr())) } != 3 {
                    return Err(invalid());
                }
            }
            if create && matches!(component, Component::Normal(_)) {
                let user = User::current()?;
                let security = user.security(true)?;
                let attrs = attributes(&security);
                let name = wide(&current)?;
                if let Err(error) = unsafe { CreateDirectoryW(PCWSTR(name.as_ptr()), Some(&attrs)) }
                {
                    let error = win(error);
                    if error.kind() != io::ErrorKind::AlreadyExists {
                        return Err(error);
                    }
                }
            }
            let file = open(&current, true, OPEN_EXISTING, leaf)?;
            if leaf {
                let mut filesystem = [0u16; 32];
                unsafe {
                    GetVolumeInformationByHandleW(
                        handle(&file),
                        None,
                        None,
                        None,
                        None,
                        Some(&mut filesystem),
                    )
                    .map_err(win)?;
                }
                let end = filesystem
                    .iter()
                    .position(|c| *c == 0)
                    .ok_or_else(invalid)?;
                if String::from_utf16_lossy(&filesystem[..end]) != "NTFS" {
                    return Err(invalid());
                }
            }
            pins.push(file);
        }
        Ok(Self { path, _pins: pins })
    }
}
pub(super) fn file(path: &Path, create: bool) -> io::Result<File> {
    open(
        path,
        false,
        if create { OPEN_ALWAYS } else { OPEN_EXISTING },
        true,
    )
}
pub(super) fn temporary(path: &Path) -> io::Result<File> {
    open(path, false, CREATE_NEW, true)
}
pub(super) fn replace(from: &Path, to: &Path) -> io::Result<()> {
    let from = wide(from)?;
    let to = wide(to)?;
    unsafe {
        MoveFileExW(
            PCWSTR(from.as_ptr()),
            PCWSTR(to.as_ptr()),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
        .map_err(win)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn broad_or_unprotected_dacl_is_rejected_without_repair() {
        let root = std::env::temp_dir().join(format!("claim-acl-{}", uuid::Uuid::new_v4()));
        let guard = Directory::open(&root, true).unwrap();
        let user = User::current().unwrap();
        let mut sid_text = PWSTR::null();
        unsafe {
            ConvertSidToStringSidW(user.sid(), &mut sid_text).unwrap();
        }
        let allocation = Allocation(sid_text.0.cast());
        let sid = unsafe { sid_text.to_string().unwrap() };
        for (index, dacl) in ["D:P(A;;FA;;;WD)".to_string(), format!("D:(A;;FA;;;{sid})")]
            .into_iter()
            .enumerate()
        {
            let path = root.join(format!("unsafe-{index}"));
            let name = wide(&path).unwrap();
            let sddl = wide(Path::new(&format!("O:{sid}{dacl}"))).unwrap();
            let mut descriptor = PSECURITY_DESCRIPTOR::default();
            unsafe {
                ConvertStringSecurityDescriptorToSecurityDescriptorW(
                    PCWSTR(sddl.as_ptr()),
                    1,
                    &mut descriptor,
                    None,
                )
                .unwrap();
            }
            let descriptor = Allocation(descriptor.0);
            let attrs = attributes(&descriptor);
            let raw = unsafe {
                CreateFileW(
                    PCWSTR(name.as_ptr()),
                    FILE_GENERIC_READ.0 | FILE_GENERIC_WRITE.0,
                    FILE_SHARE_READ | FILE_SHARE_WRITE,
                    Some(&attrs),
                    CREATE_NEW,
                    FILE_FLAG_OPEN_REPARSE_POINT,
                    None,
                )
                .unwrap()
            };
            let created = unsafe { File::from_raw_handle(raw.0) };
            assert!(validate(&created, false, true).is_err());
            drop(created);
            assert!(file(&path, false).is_err());
            // A second refusal proves the attempted open did not repair the ACL.
            assert!(file(&path, false).is_err());
        }
        drop(allocation);
        drop(guard);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn pinned_ancestor_cannot_be_renamed() {
        let root = std::env::temp_dir().join(format!("claim-ancestor-{}", uuid::Uuid::new_v4()));
        let child = root.join("wallet").join("claim");
        let guard = Directory::open(&child, true).unwrap();
        let moved = root.with_extension("moved");
        assert!(std::fs::rename(&root, &moved).is_err());
        drop(guard);
        std::fs::rename(&root, &moved).unwrap();
        std::fs::remove_dir_all(moved).unwrap();
    }
}
