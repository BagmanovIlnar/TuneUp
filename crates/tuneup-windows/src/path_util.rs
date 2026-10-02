use std::{
    ffi::{OsStr, OsString},
    fs,
    os::windows::ffi::{OsStrExt, OsStringExt},
    path::{Path, PathBuf},
    ptr,
};

use windows_sys::Win32::{
    Foundation::LocalFree, System::Environment::ExpandEnvironmentStringsW,
    UI::Shell::CommandLineToArgvW,
};

use tuneup_core::model::{normalize_path, path_is_within};

pub(crate) fn command_executable(command: &OsStr) -> Option<PathBuf> {
    let expanded = expand_environment(command);
    let wide = to_wide(&expanded);
    let mut argument_count = 0;
    // SAFETY: `wide` is NUL-terminated and the output pointer is valid.
    let arguments = unsafe { CommandLineToArgvW(wide.as_ptr(), &mut argument_count) };
    if arguments.is_null() || argument_count < 1 {
        return None;
    }
    // SAFETY: The API returned at least one NUL-terminated argument. The
    // allocation is released exactly once with LocalFree.
    let path = unsafe {
        let first = *arguments;
        let length = (0..).take_while(|&index| *first.add(index) != 0).count();
        let value = OsString::from_wide(std::slice::from_raw_parts(first, length));
        LocalFree(arguments.cast());
        PathBuf::from(value)
    };
    Some(path)
}

pub(crate) fn expand_environment(value: &OsStr) -> OsString {
    let source = to_wide(value);
    // SAFETY: Source is NUL-terminated; null destination requests the size.
    let required = unsafe { ExpandEnvironmentStringsW(source.as_ptr(), ptr::null_mut(), 0) };
    if required == 0 {
        return value.to_owned();
    }
    let mut destination = vec![0u16; required as usize];
    // SAFETY: The destination has the size returned by the first call.
    let written =
        unsafe { ExpandEnvironmentStringsW(source.as_ptr(), destination.as_mut_ptr(), required) };
    if written == 0 || written > required {
        return value.to_owned();
    }
    destination.truncate(written.saturating_sub(1) as usize);
    OsString::from_wide(&destination)
}

pub(crate) fn canonical_path(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

pub(crate) fn exact_group_match(path: &Path, install_root: &Path) -> bool {
    path_is_within(&canonical_path(path), &canonical_path(install_root))
}

pub(crate) fn protected_system_path(path: &Path) -> bool {
    let normalized = normalize_path(&canonical_path(path));
    std::env::var_os("SystemRoot")
        .map(PathBuf::from)
        .is_some_and(|root| {
            let root = normalize_path(&canonical_path(&root));
            normalized == root || normalized.starts_with(&(root + "/"))
        })
}

pub(crate) fn to_wide(value: &OsStr) -> Vec<u16> {
    value.encode_wide().chain(std::iter::once(0)).collect()
}

pub(crate) fn wide_ptr_to_os_string(pointer: *const u16) -> OsString {
    if pointer.is_null() {
        return OsString::new();
    }
    // SAFETY: Callers provide a Windows API NUL-terminated string pointer.
    unsafe {
        let length = (0..).take_while(|&index| *pointer.add(index) != 0).count();
        OsString::from_wide(std::slice::from_raw_parts(pointer, length))
    }
}
