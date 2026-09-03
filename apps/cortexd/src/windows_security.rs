//! Narrow, audited Windows security-descriptor boundary for the local named pipe.
//!
//! This is the sole `unsafe` exception in `cortexd`: `windows-sys` exposes the Win32 APIs as
//! raw pointers, while Tokio requires a valid `SECURITY_ATTRIBUTES` pointer for pipe creation.

use std::{ffi::c_void, mem::size_of, ptr::null_mut};

use windows_sys::Win32::Security::Authorization::{SE_FILE_OBJECT, SetNamedSecurityInfoW};
use windows_sys::Win32::{
    Foundation::{CloseHandle, HANDLE, LocalFree},
    Security::{
        Authorization::{
            ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
        },
        DACL_SECURITY_INFORMATION, GetSecurityDescriptorDacl, GetTokenInformation,
        PROTECTED_DACL_SECURITY_INFORMATION, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER,
        TokenUser,
    },
    System::Threading::{GetCurrentProcess, OpenProcessToken},
};

/// RAII attributes with a protected DACL granting pipe access only to the current user SID.
pub struct CurrentUserPipeSecurity {
    descriptor: *mut c_void,
    attributes: SECURITY_ATTRIBUTES,
}

impl CurrentUserPipeSecurity {
    /// Builds a protected current-user DACL.
    pub fn new() -> Result<Self, ()> {
        let mut token: HANDLE = null_mut();
        // SAFETY: token output is valid and is closed on every successful-open path.
        if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &raw mut token) } == 0 {
            return Err(());
        }
        let result = current_user_sddl(token).and_then(|sddl| security_from_sddl(&sddl));
        // SAFETY: token is the handle returned by OpenProcessToken and is no longer used.
        unsafe { CloseHandle(token) };
        result
    }

    #[must_use]
    pub fn as_raw_mut(&mut self) -> *mut c_void {
        (&raw mut self.attributes).cast()
    }

    /// Creates a pipe while the owned descriptor remains valid for Tokio's call.
    pub fn create_server(
        &mut self,
        options: &tokio::net::windows::named_pipe::ServerOptions,
        name: &str,
    ) -> std::io::Result<tokio::net::windows::named_pipe::NamedPipeServer> {
        // SAFETY: `self` owns a valid SECURITY_ATTRIBUTES until Tokio returns from this call.
        unsafe { options.create_with_security_attributes_raw(name, self.as_raw_mut()) }
    }
}

/// Creates one server with a DACL restricted to the current Windows user.
pub fn create_current_user_server(
    options: &tokio::net::windows::named_pipe::ServerOptions,
    name: &str,
) -> std::io::Result<tokio::net::windows::named_pipe::NamedPipeServer> {
    let mut security = CurrentUserPipeSecurity::new()
        .map_err(|()| std::io::Error::other("current-user pipe DACL unavailable"))?;
    security.create_server(options, name)
}

/// Applies the same protected current-user DACL to a pairing enrollment artifact.
pub fn restrict_current_user_file(path: &std::path::Path) -> Result<(), ()> {
    use std::os::windows::ffi::OsStrExt;

    let security = CurrentUserPipeSecurity::new()?;
    let mut present = 0;
    let mut defaulted = 0;
    let mut dacl = null_mut();
    // SAFETY: security owns a valid descriptor and all out-pointers are valid for this call.
    if unsafe {
        GetSecurityDescriptorDacl(
            security.descriptor.cast(),
            &raw mut present,
            &raw mut dacl,
            &raw mut defaulted,
        )
    } == 0
        || present == 0
        || dacl.is_null()
    {
        return Err(());
    }
    let mut wide: Vec<u16> = path.as_os_str().encode_wide().collect();
    wide.push(0);
    // SAFETY: wide is NUL-terminated, dacl points into the live descriptor, and the path names
    // the just-created enrollment file.
    let status = unsafe {
        SetNamedSecurityInfoW(
            wide.as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            null_mut(),
            null_mut(),
            dacl,
            null_mut(),
        )
    };
    (status == 0).then_some(()).ok_or(())
}

impl Drop for CurrentUserPipeSecurity {
    fn drop(&mut self) {
        if !self.descriptor.is_null() {
            // SAFETY: descriptor is allocated by ConvertStringSecurityDescriptorToSecurityDescriptorW.
            unsafe { LocalFree(self.descriptor) };
        }
    }
}

fn current_user_sddl(token: HANDLE) -> Result<Vec<u16>, ()> {
    let mut bytes = 0_u32;
    // SAFETY: this null-buffer call is the documented size query.
    unsafe { GetTokenInformation(token, TokenUser, null_mut(), 0, &raw mut bytes) };
    if bytes < u32::try_from(size_of::<TOKEN_USER>()).map_err(|_| ())? {
        return Err(());
    }
    let words = usize::try_from(bytes)
        .map_err(|_| ())?
        .div_ceil(size_of::<usize>());
    let mut buffer = vec![0_usize; words];
    // SAFETY: buffer has the exact byte count returned by the first call.
    if unsafe {
        GetTokenInformation(
            token,
            TokenUser,
            buffer.as_mut_ptr().cast(),
            bytes,
            &raw mut bytes,
        )
    } == 0
    {
        return Err(());
    }
    // SAFETY: successful TokenUser output starts with TOKEN_USER.
    let user = unsafe { &*(buffer.as_ptr().cast::<TOKEN_USER>()) };
    let mut sid_text = null_mut();
    // SAFETY: SID came from the authenticated process token; output is LocalFree'd below.
    if unsafe { ConvertSidToStringSidW(user.User.Sid, &raw mut sid_text) } == 0 {
        return Err(());
    }
    let sid = wide_string(sid_text);
    // SAFETY: ConvertSidToStringSidW allocates sid_text with LocalAlloc.
    unsafe { LocalFree(sid_text.cast()) };
    let mut sddl = "D:P(A;;GA;;;".encode_utf16().collect::<Vec<_>>();
    sddl.extend_from_slice(&sid[..sid.len().saturating_sub(1)]);
    sddl.extend(")".encode_utf16());
    sddl.push(0);
    Ok(sddl)
}

fn security_from_sddl(sddl: &[u16]) -> Result<CurrentUserPipeSecurity, ()> {
    let mut descriptor = null_mut();
    // SAFETY: SDDL is NUL-terminated UTF-16 and all output pointers are valid.
    if unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            1,
            &raw mut descriptor,
            null_mut(),
        )
    } == 0
    {
        return Err(());
    }
    Ok(CurrentUserPipeSecurity {
        descriptor: descriptor.cast(),
        attributes: SECURITY_ATTRIBUTES {
            nLength: u32::try_from(size_of::<SECURITY_ATTRIBUTES>()).map_err(|_| ())?,
            lpSecurityDescriptor: descriptor,
            bInheritHandle: 0,
        },
    })
}

fn wide_string(pointer: *const u16) -> Vec<u16> {
    let mut length = 0_usize;
    // SAFETY: pointer is a NUL-terminated string returned by Win32.
    unsafe {
        while *pointer.add(length) != 0 {
            length += 1;
        }
    }
    // SAFETY: the NUL-terminated range is valid for that Win32 string.
    unsafe { std::slice::from_raw_parts(pointer, length + 1).to_vec() }
}
