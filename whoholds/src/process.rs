//! 进程表：一次 Toolhelp 快照拿到全部 PID / 进程名 / 父 PID。
use crate::winapi::{self, ProcessEntry32W};
use std::collections::HashMap;

pub(crate) struct ProcessInfo {
    pub name: String,
}

pub(crate) fn process_map() -> std::io::Result<HashMap<u32, ProcessInfo>> {
    let snap = unsafe { winapi::CreateToolhelp32Snapshot(winapi::TH32CS_SNAPPROCESS, 0) };
    if snap.is_null() || snap == winapi::INVALID_HANDLE_VALUE {
        return Err(std::io::Error::last_os_error());
    }
    let mut map = HashMap::new();
    let mut e: ProcessEntry32W = unsafe { std::mem::zeroed() };
    e.dwSize = std::mem::size_of::<ProcessEntry32W>() as u32;
    let mut ok = unsafe { winapi::Process32FirstW(snap, &mut e) };
    while ok != 0 {
        let name = winapi::from_wide(&e.szExeFile);
        map.insert(e.th32ProcessID, ProcessInfo { name });
        ok = unsafe { winapi::Process32NextW(snap, &mut e) };
    }
    unsafe { winapi::CloseHandle(snap) };
    Ok(map)
}
