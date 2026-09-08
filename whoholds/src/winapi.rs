//! 集中声明本工具用到的原生 API（kernel32 / ntdll）。
//! 不引入 windows-sys：所需 API 面小且 ABI 稳定，手写声明可避免其版本间类型漂移。
#![allow(non_snake_case)]

use std::ffi::c_void;

/// 原生 HANDLE
pub(crate) type H = *mut c_void;

pub(crate) const INVALID_HANDLE_VALUE: H = -1isize as H;

// ---- 常量 ----
pub(crate) const GENERIC_READ: u32 = 0x8000_0000;
pub(crate) const FILE_SHARE_READ: u32 = 0x1;
pub(crate) const FILE_SHARE_WRITE: u32 = 0x2;
pub(crate) const FILE_SHARE_DELETE: u32 = 0x4;
pub(crate) const FILE_SHARE_ALL: u32 = FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE;
pub(crate) const OPEN_EXISTING: u32 = 3;
pub(crate) const FILE_ATTRIBUTE_NORMAL: u32 = 0x80;
pub(crate) const PROCESS_DUP_HANDLE: u32 = 0x40;
pub(crate) const DUPLICATE_SAME_ACCESS: u32 = 0x2;
pub(crate) const TOKEN_QUERY: u32 = 0x8;
pub(crate) const TOKEN_ADJUST_PRIVILEGES: u32 = 0x20;
pub(crate) const SE_PRIVILEGE_ENABLED: u32 = 0x2;
pub(crate) const ERROR_NOT_ALL_ASSIGNED: u32 = 1300;
pub(crate) const TH32CS_SNAPPROCESS: u32 = 0x2;
pub(crate) const CP_UTF8: u32 = 65001;

// NTSTATUS
pub(crate) const STATUS_SUCCESS: i32 = 0;
pub(crate) const STATUS_BUFFER_OVERFLOW: i32 = 0x8000_0005u32 as i32;
pub(crate) const STATUS_INFO_LENGTH_MISMATCH: i32 = 0xC000_0004u32 as i32;
pub(crate) const STATUS_OBJECT_NAME_NOT_FOUND: i32 = 0xC000_0034u32 as i32;

// ---- 原生结构体（布局静态断言在文件末尾） ----

/// PROCESSENTRY32W（x64：size = 568）
#[repr(C)]
pub(crate) struct ProcessEntry32W {
    pub dwSize: u32,
    pub cntUsage: u32,
    pub th32ProcessID: u32,
    pub th32DefaultHeapID: usize,
    pub th32ModuleID: u32,
    pub cntThreads: u32,
    pub th32ParentProcessID: u32,
    pub pcPriClassBase: i32,
    pub dwFlags: u32,
    pub szExeFile: [u16; 260],
}

/// LUID（两个 4 字节整数，对齐 4——用 i64 会把对齐抬到 8 导致布局错误）
#[repr(C)]
#[derive(Clone, Copy)]
pub(crate) struct Luid {
    pub low_part: u32,
    pub high_part: i32,
}

#[repr(C)]
pub(crate) struct LuidAndAttributes {
    pub luid: Luid,
    pub attributes: u32,
}

/// TOKEN_PRIVILEGES（x64：size = 16，Privileges[0].Luid 在偏移 4）
#[repr(C)]
pub(crate) struct TokenPrivileges {
    pub privilege_count: u32,
    pub privileges: [LuidAndAttributes; 1],
}

/// UNICODE_STRING（x64：size = 16）
#[repr(C)]
pub(crate) struct UnicodeString {
    pub Length: u16,
    pub MaximumLength: u16,
    pub Buffer: *mut u16,
}

#[link(name = "kernel32")]
unsafe extern "system" {
    pub(crate) fn CloseHandle(hObject: H) -> i32;
    pub(crate) fn GetLastError() -> u32;
    pub(crate) fn CreateFileW(
        lpFileName: *const u16,
        dwDesiredAccess: u32,
        dwShareMode: u32,
        lpSecurityAttributes: *const c_void,
        dwCreationDisposition: u32,
        dwFlagsAndAttributes: u32,
        hTemplateFile: H,
    ) -> H;
    pub(crate) fn GetCurrentProcess() -> H;
    pub(crate) fn GetCurrentProcessId() -> u32;
    pub(crate) fn OpenProcess(dwDesiredAccess: u32, bInheritHandle: i32, dwProcessId: u32) -> H;
    pub(crate) fn DuplicateHandle(
        hSourceProcessHandle: H,
        hSourceHandle: H,
        hTargetProcessHandle: H,
        lpTargetHandle: *mut H,
        dwDesiredAccess: u32,
        bInheritHandle: i32,
        dwOptions: u32,
    ) -> i32;
    pub(crate) fn GetLogicalDriveStringsW(nBufferLength: u32, lpBuffer: *mut u16) -> u32;
    pub(crate) fn GetFullPathNameW(
        lpFileName: *const u16,
        nBufferLength: u32,
        lpBuffer: *mut u16,
        lpFilePart: *mut *mut u16,
    ) -> u32;
    pub(crate) fn GetLongPathNameW(
        lpszShortPath: *const u16,
        lpszLongPath: *mut u16,
        cchBuffer: u32,
    ) -> u32;
    pub(crate) fn QueryDosDeviceW(
        lpDeviceName: *const u16,
        lpTargetPath: *mut u16,
        ucchMax: u32,
    ) -> u32;
    pub(crate) fn CreateToolhelp32Snapshot(dwFlags: u32, th32ProcessID: u32) -> H;
    pub(crate) fn Process32FirstW(hSnapshot: H, lppe: *mut ProcessEntry32W) -> i32;
    pub(crate) fn Process32NextW(hSnapshot: H, lppe: *mut ProcessEntry32W) -> i32;
    pub(crate) fn GetConsoleOutputCP() -> u32;
    pub(crate) fn SetConsoleOutputCP(wCodePageID: u32) -> i32;
}

// Token / 权限 API 实际位于 advapi32
#[link(name = "advapi32")]
unsafe extern "system" {
    pub(crate) fn OpenProcessToken(
        ProcessHandle: H,
        DesiredAccess: u32,
        TokenHandle: *mut H,
    ) -> i32;
    pub(crate) fn LookupPrivilegeValueW(
        lpSystemName: *const u16,
        lpName: *const u16,
        lpLuid: *mut i64,
    ) -> i32;
    pub(crate) fn AdjustTokenPrivileges(
        TokenHandle: H,
        DisableAllPrivileges: i32,
        NewState: *const TokenPrivileges,
        BufferLength: u32,
        PreviousState: *mut c_void,
        ReturnLength: *mut u32,
    ) -> i32;
}

#[link(name = "ntdll")]
unsafe extern "system" {
    pub(crate) fn NtQuerySystemInformation(
        SystemInformationClass: u32,
        SystemInformation: *mut c_void,
        SystemInformationLength: u32,
        ReturnLength: *mut u32,
    ) -> i32;
    pub(crate) fn NtQueryObject(
        Handle: H,
        ObjectInformationClass: u32,
        ObjectInformation: *mut c_void,
        ObjectInformationLength: u32,
        ReturnLength: *mut u32,
    ) -> i32;
}

// ---- 布局守卫（仅支持 64 位 Windows） ----
const _: () = assert!(std::mem::size_of::<ProcessEntry32W>() == 568);
const _: () = assert!(std::mem::size_of::<TokenPrivileges>() == 16);
const _: () = assert!(std::mem::size_of::<UnicodeString>() == 16);

// ---- 辅助 ----

pub(crate) fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

pub(crate) fn from_wide(s: &[u16]) -> String {
    let end = s.iter().position(|&c| c == 0).unwrap_or(s.len());
    String::from_utf16_lossy(&s[..end])
}

/// 尽力启用 SeDebugPrivilege：成功返回 None，失败返回失败步骤（诊断用）
pub(crate) fn enable_debug_privilege() -> Option<&'static str> {
    unsafe {
        let mut token: H = std::ptr::null_mut();
        if OpenProcessToken(
            GetCurrentProcess(),
            TOKEN_ADJUST_PRIVILEGES | TOKEN_QUERY,
            &mut token,
        ) == 0
        {
            return Some("OpenProcessToken");
        }
        let name = to_wide("SeDebugPrivilege");
        let mut luid: i64 = 0;
        if LookupPrivilegeValueW(std::ptr::null(), name.as_ptr(), &mut luid) == 0 {
            CloseHandle(token);
            return Some("LookupPrivilegeValueW");
        }
        let tp = TokenPrivileges {
            privilege_count: 1,
            privileges: [LuidAndAttributes {
                luid: Luid {
                    low_part: luid as u32,
                    high_part: (luid >> 32) as i32,
                },
                attributes: SE_PRIVILEGE_ENABLED,
            }],
        };
        let ok =
            AdjustTokenPrivileges(token, 0, &tp, 0, std::ptr::null_mut(), std::ptr::null_mut());
        let err = GetLastError();
        CloseHandle(token);
        if ok == 0 {
            return Some("AdjustTokenPrivileges");
        }
        // 成功返回但 GetLastError==ERROR_NOT_ALL_ASSIGNED 表示权限实际未授予
        if err == ERROR_NOT_ALL_ASSIGNED {
            return Some("NOT_ALL_ASSIGNED");
        }
        None
    }
}
