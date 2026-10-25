//! Filesystem helpers for security-sensitive files.
//!
//! Private keys, the trusted-peer list, and the application config must not be
//! readable by other local users. On Unix that means creating the file with a
//! restrictive mode and re-tightening the mode of files that already exist. On
//! Windows it means replacing the DACL with one that grants only the current
//! user and the system.

use std::fs;
use std::path::Path;

/// Writes `data` to `path`, restricting the file to the current user.
///
/// Existing files are truncated and their permissions are tightened. Unsigned
/// platforms fall back to a plain write.
pub(crate) fn write_private_file(path: &Path, data: &[u8]) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;

        let mut file = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)?;
        file.write_all(data)
    }

    #[cfg(windows)]
    {
        fs::write(path, data)?;
        windows_acl::apply_private_dacl(path)
    }

    #[cfg(not(any(unix, windows)))]
    {
        fs::write(path, data)
    }
}

/// Tightens the permissions of an existing path to the current user.
///
/// Used to remediate files that were created before private-file handling was
/// introduced. Failures are returned so the caller can decide whether to log
/// and continue.
pub(crate) fn restrict_permissions(path: &Path, mode: u32) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        fs::set_permissions(path, fs::Permissions::from_mode(mode))
    }

    #[cfg(windows)]
    {
        let _ = mode;
        windows_acl::apply_private_dacl(path)
    }

    #[cfg(not(any(unix, windows)))]
    {
        let _ = (path, mode);
        Ok(())
    }
}

#[allow(dead_code)]
pub(crate) fn is_subpath_case_insensitive(path: &Path, root: &Path) -> bool {
    let split_components = |p: &Path| -> Vec<String> {
        let s = p.to_string_lossy();
        let stripped = s
            .strip_prefix(r"\\?\")
            .or_else(|| s.strip_prefix("//?/"))
            .unwrap_or(&s);
        stripped
            .split(|c| c == '/' || c == '\\')
            .filter(|seg| !seg.is_empty())
            .map(|seg| seg.to_string())
            .collect()
    };

    let path_comps = split_components(path);
    let root_comps = split_components(root);
    if path_comps.len() < root_comps.len() {
        return false;
    }
    for (p, r) in path_comps.iter().zip(root_comps.iter()) {
        if !p.eq_ignore_ascii_case(r) {
            return false;
        }
    }
    true
}

#[cfg(windows)]
mod windows_acl {
    use std::ffi::OsStr;
    use std::mem::size_of;
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;

    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, LocalFree};
    use windows_sys::Win32::Security::Authorization::{
        ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
        SDDL_REVISION_1,
    };
    use windows_sys::Win32::Security::{
        DACL_SECURITY_INFORMATION, GetLengthSid, GetTokenInformation, PSID, SetFileSecurityW,
        TOKEN_QUERY, TOKEN_USER, TokenUser,
    };
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
    use windows_sys::core::PWSTR;

    /// Replaces the path's DACL with one that grants only the owning user and
    /// the system, matching the Unix `0600` treatment. Directories get
    /// inheritable entries so files created inside them start out restricted.
    pub(super) fn apply_private_dacl(path: &Path) -> std::io::Result<()> {
        let user = current_user_sid_string()
            .ok_or_else(|| std::io::Error::other("failed to resolve the current user SID"))?;
        set_dacl_from_sddl(path, &private_dacl_sddl(&user, path.is_dir()))
    }

    /// Descriptor written for private files and directories. `P` stops the
    /// parent's (looser) entries from applying, so nothing else reaches the key.
    pub(super) fn private_dacl_sddl(user_sid: &str, is_directory: bool) -> String {
        if is_directory {
            format!("D:P(A;OICI;FA;;;SY)(A;OICI;FA;;;{user_sid})")
        } else {
            format!("D:P(A;;FA;;;SY)(A;;FA;;;{user_sid})")
        }
    }

    fn set_dacl_from_sddl(path: &Path, sddl: &str) -> std::io::Result<()> {
        let sddl_wide = wide(OsStr::new(sddl));
        let mut descriptor = std::ptr::null_mut();
        let converted = unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl_wide.as_ptr(),
                SDDL_REVISION_1,
                &mut descriptor,
                std::ptr::null_mut(),
            )
        };
        if converted == 0 || descriptor.is_null() {
            return Err(std::io::Error::last_os_error());
        }

        let path_wide = wide(path.as_os_str());
        let applied =
            unsafe { SetFileSecurityW(path_wide.as_ptr(), DACL_SECURITY_INFORMATION, descriptor) };
        let error = std::io::Error::last_os_error();
        unsafe { LocalFree(descriptor as *mut core::ffi::c_void) };

        if applied == 0 { Err(error) } else { Ok(()) }
    }

    pub(super) fn current_user_sid_string() -> Option<String> {
        let mut token: HANDLE = std::ptr::null_mut();
        let opened = unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) };
        if opened == 0 || token.is_null() {
            return None;
        }
        let sid = token_user_sid(token);
        unsafe { CloseHandle(token) };
        sid.and_then(|sid| sid_to_string(&sid))
    }

    /// Reads the user SID out of an access token.
    fn token_user_sid(token: HANDLE) -> Option<Vec<u8>> {
        let mut needed = 0u32;
        unsafe {
            GetTokenInformation(token, TokenUser, std::ptr::null_mut(), 0, &mut needed);
        }
        if needed == 0 {
            return None;
        }
        // TOKEN_USER holds pointers, so the buffer must be pointer aligned.
        let words = (needed as usize).div_ceil(size_of::<usize>());
        let mut buffer = vec![0usize; words];
        let ok = unsafe {
            GetTokenInformation(
                token,
                TokenUser,
                buffer.as_mut_ptr() as *mut core::ffi::c_void,
                needed,
                &mut needed,
            )
        };
        if ok == 0 {
            return None;
        }
        let user = unsafe { &*(buffer.as_ptr() as *const TOKEN_USER) };
        let sid = user.User.Sid;
        if sid.is_null() {
            return None;
        }
        let length = unsafe { GetLengthSid(sid) } as usize;
        if length == 0 {
            return None;
        }
        Some(unsafe { std::slice::from_raw_parts(sid as *const u8, length) }.to_vec())
    }

    fn sid_to_string(sid: &[u8]) -> Option<String> {
        let mut text: PWSTR = std::ptr::null_mut();
        let ok = unsafe { ConvertSidToStringSidW(sid.as_ptr() as PSID, &mut text) };
        if ok == 0 || text.is_null() {
            return None;
        }
        let value = unsafe {
            let mut length = 0usize;
            while *text.add(length) != 0 {
                length += 1;
            }
            String::from_utf16_lossy(std::slice::from_raw_parts(text, length))
        };
        unsafe { LocalFree(text as *mut core::ffi::c_void) };
        Some(value)
    }

    pub(super) fn wide(value: &OsStr) -> Vec<u16> {
        value.encode_wide().chain(std::iter::once(0)).collect()
    }
}

#[cfg(all(test, windows))]
mod windows_tests {
    use super::*;
    use crate::core::secure_fs::windows_acl::{current_user_sid_string, private_dacl_sddl, wide};
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Authorization::{
        ConvertSecurityDescriptorToStringSecurityDescriptorW, GetNamedSecurityInfoW,
        SDDL_REVISION_1, SE_FILE_OBJECT,
    };
    use windows_sys::Win32::Security::{DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR};
    use windows_sys::core::PWSTR;

    /// Reads the DACL Windows actually stored for `path`, as SDDL text.
    fn stored_dacl(path: &Path) -> String {
        let path_wide = wide(path.as_os_str());
        let mut descriptor: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
        let mut dacl = std::ptr::null_mut();
        let status = unsafe {
            GetNamedSecurityInfoW(
                path_wide.as_ptr(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut dacl,
                std::ptr::null_mut(),
                &mut descriptor,
            )
        };
        assert_eq!(status, 0, "GetNamedSecurityInfoW failed with {status}");

        let mut text: PWSTR = std::ptr::null_mut();
        let converted = unsafe {
            ConvertSecurityDescriptorToStringSecurityDescriptorW(
                descriptor,
                SDDL_REVISION_1,
                DACL_SECURITY_INFORMATION,
                &mut text,
                std::ptr::null_mut(),
            )
        };
        assert_ne!(converted, 0, "SDDL conversion failed with {converted}");

        let value = unsafe {
            let mut length = 0usize;
            while *text.add(length) != 0 {
                length += 1;
            }
            String::from_utf16_lossy(std::slice::from_raw_parts(text, length))
        };
        unsafe {
            LocalFree(text as *mut core::ffi::c_void);
            LocalFree(descriptor);
        }
        value
    }

    /// Asserts the descriptor grants exactly the current user and the system.
    fn assert_owner_and_system_only(sddl: &str) {
        let user = current_user_sid_string().expect("current user SID resolves");
        assert!(sddl.contains(&format!(";;;{user})")), "owner keeps access: {sddl}");
        assert!(sddl.contains(";;;SY)"), "system keeps access: {sddl}");
        assert!(!sddl.contains(";;;AU)"), "authenticated users must be denied: {sddl}");
        assert!(!sddl.contains(";;;WD)"), "everyone must be denied: {sddl}");
        assert!(!sddl.contains(";;;BU)"), "builtin users must be denied: {sddl}");
        assert!(
            !sddl.contains("ID)"),
            "the descriptor must not carry inherited entries: {sddl}"
        );
    }

    #[test]
    fn private_files_are_restricted_to_the_owner_and_system() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("known_peers.toml");

        write_private_file(&path, b"secret").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"secret");

        assert_owner_and_system_only(&stored_dacl(&path));
    }

    #[test]
    fn existing_files_are_tightened_by_restrict_permissions() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("self.key");
        fs::write(&path, b"secret").unwrap();

        restrict_permissions(&path, 0o600).unwrap();

        assert_owner_and_system_only(&stored_dacl(&path));
    }

    #[test]
    fn directory_entries_are_inheritable() {
        let dir = tempfile::TempDir::new().unwrap();

        restrict_permissions(dir.path(), 0o700).unwrap();

        let sddl = stored_dacl(dir.path());
        assert_owner_and_system_only(&sddl);
        assert!(sddl.contains("OICI"), "entries must inherit to children: {sddl}");
    }

    #[test]
    fn descriptor_text_targets_only_the_current_user_and_system() {
        let sddl = private_dacl_sddl("S-1-5-21-1-2-3-1001", false);
        assert_eq!(sddl, "D:P(A;;FA;;;SY)(A;;FA;;;S-1-5-21-1-2-3-1001)");

        let sddl = private_dacl_sddl("S-1-5-21-1-2-3-1001", true);
        assert_eq!(
            sddl,
            "D:P(A;OICI;FA;;;SY)(A;OICI;FA;;;S-1-5-21-1-2-3-1001)"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_subpath_case_insensitive() {
        let root = Path::new(r"C:\Program Files\ZeroG");
        let valid_sub = Path::new(r"c:\program files\zerog\zerog-input.exe");
        let outside = Path::new(r"C:\Users\Alice\Downloads\ZeroG.exe");
        let prefix_only = Path::new(r"C:\Program Files\ZeroGOther\ZeroG.exe");

        assert!(is_subpath_case_insensitive(valid_sub, root));
        assert!(!is_subpath_case_insensitive(outside, root));
        assert!(!is_subpath_case_insensitive(prefix_only, root));
    }
}
