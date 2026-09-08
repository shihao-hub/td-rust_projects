//! 全系统句柄表快照：NtQuerySystemInformation(SystemExtendedHandleInformation)。
//! 一次调用拿到所有进程的句柄条目，缓冲区内按原生布局零拷贝解析。
use crate::winapi;
use std::ffi::c_void;

pub(crate) const SYSTEM_EXTENDED_HANDLE_INFORMATION: u32 = 64;

/// SYSTEM_HANDLE_TABLE_ENTRY_EX（x64：size = 40）
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub(crate) struct HandleEntryEx {
    pub object: usize,
    pub pid: usize,
    pub handle: usize,
    pub access: u32,
    pub back_trace_index: u16,
    pub type_index: u16,
    pub attributes: u32,
}
const _: () = assert!(std::mem::size_of::<HandleEntryEx>() == 40);

pub(crate) struct HandleSnapshot {
    /// 8 字节对齐的存储（头部 16 字节：NumberOfHandles + Reserved，随后为条目数组）
    storage: Vec<usize>,
    count: usize,
}

impl HandleSnapshot {
    pub(crate) fn take() -> std::io::Result<Self> {
        const HEADER: usize = 16;
        const ENTRY: usize = std::mem::size_of::<HandleEntryEx>();
        // 直接按 16MB 起步：26 万句柄 × 40B ≈ 10.4MB，避免增长重试的整轮重复拷贝
        let mut bytes: usize = 16 << 20;
        let mut attempt = 0;
        loop {
            let mut storage = vec![0usize; bytes / 8];
            let mut ret_len = 0u32;
            let status = unsafe {
                winapi::NtQuerySystemInformation(
                    SYSTEM_EXTENDED_HANDLE_INFORMATION,
                    storage.as_mut_ptr().cast::<c_void>(),
                    (storage.len() * std::mem::size_of::<usize>()) as u32,
                    &mut ret_len,
                )
            };
            match status {
                winapi::STATUS_SUCCESS => {
                    let count = storage[0];
                    if HEADER + count * ENTRY <= storage.len() * 8 {
                        return Ok(Self { storage, count });
                    }
                    // 极罕见：返回成功但条目数超出缓冲区，按实际需要放大重试
                    bytes = HEADER + count * ENTRY + 65536;
                }
                winapi::STATUS_INFO_LENGTH_MISMATCH => {
                    // 枚举期间句柄增删导致长度不定：放大缓冲重试
                    bytes = ((bytes * 2).max(ret_len as usize + 65536) + 65535) & !65535;
                }
                code => {
                    let mut msg = format!(
                        "NtQuerySystemInformation 失败: NTSTATUS 0x{:08X}",
                        code as u32
                    );
                    if code as u32 == 0xC000_0022 {
                        msg.push_str("（拒绝访问，请尝试以管理员运行）");
                    }
                    return Err(std::io::Error::other(msg));
                }
            }
            attempt += 1;
            if attempt >= 8 {
                return Err(std::io::Error::other(
                    "句柄表快照多次重试仍不稳定，请稍后重试",
                ));
            }
        }
    }

    pub(crate) fn total(&self) -> usize {
        self.count
    }

    pub(crate) fn entries(&self) -> &[HandleEntryEx] {
        // 布局：[NumberOfHandles: usize][Reserved: usize][entries...]
        unsafe {
            std::slice::from_raw_parts(
                self.storage.as_ptr().add(2).cast::<HandleEntryEx>(),
                self.count,
            )
        }
    }
}
