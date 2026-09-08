//! 动态探测 File 对象的 ObjectTypeIndex：该索引随 Windows 版本变化，必须运行时校准。
//! 做法：在自己进程里打开一个已知文件，去句柄表快照里找这个句柄，读它的 type_index。
use crate::snapshot::HandleSnapshot;
use crate::winapi::{self, H};

pub(crate) struct ProbeFile {
    h: H,
    /// 句柄值，供候选过滤时排除自身
    pub value: usize,
}

impl ProbeFile {
    pub(crate) fn open() -> std::io::Result<Self> {
        let exe = std::env::current_exe()?;
        let wide = winapi::to_wide(&exe.to_string_lossy());
        let h = unsafe {
            winapi::CreateFileW(
                wide.as_ptr(),
                winapi::GENERIC_READ,
                winapi::FILE_SHARE_ALL,
                std::ptr::null(),
                winapi::OPEN_EXISTING,
                winapi::FILE_ATTRIBUTE_NORMAL,
                std::ptr::null_mut(),
            )
        };
        if h.is_null() || h == winapi::INVALID_HANDLE_VALUE {
            return Err(std::io::Error::last_os_error());
        }
        Ok(Self {
            h,
            value: h as usize,
        })
    }

    pub(crate) fn type_index_in(&self, snap: &HandleSnapshot) -> std::io::Result<u16> {
        let pid = unsafe { winapi::GetCurrentProcessId() } as usize;
        snap.entries()
            .iter()
            .find(|e| e.pid == pid && e.handle == self.value)
            .map(|e| e.type_index)
            .ok_or_else(|| std::io::Error::other("校准句柄未出现在快照中"))
    }

    pub(crate) fn close(self) {
        unsafe { winapi::CloseHandle(self.h) };
    }
}
