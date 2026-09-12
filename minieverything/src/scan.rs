//! 全量扫描：jwalk 并行遍历 + 目录 file_ref 采集。

use crate::index::IndexEntry;
use crate::volume::to_wide;
use std::collections::HashMap;
use windows::core::PCWSTR;
use windows::Win32::Foundation::{CloseHandle, GENERIC_READ};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION,
    FILE_FLAG_BACKUP_SEMANTICS, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
    OPEN_EXISTING,
};

/// 扫描结果
pub struct ScanResult {
    pub entries: Vec<IndexEntry>,
    /// 目录 file_ref -> 路径
    pub dirs: HashMap<u64, String>,
    /// 因权限等原因被跳过的条目数
    pub skipped: usize,
}

/// 并行遍历一个根目录（卷根或自定义目录），收集全部文件与目录。
/// 根自身不计入条目；无法访问的条目跳过并计数。
pub fn scan_root(root: &str) -> ScanResult {
    let mut entries = Vec::new();
    let mut skipped = 0usize;

    for entry in jwalk::WalkDir::new(root).follow_links(false) {
        match entry {
            Ok(e) => {
                if e.depth() == 0 {
                    continue; // 根自身不入索引
                }
                let is_dir = e.file_type().is_dir();
                entries.push(IndexEntry {
                    path: normalize_path(&e.path().to_string_lossy()),
                    is_dir,
                });
            }
            Err(_) => skipped += 1,
        }
    }

    let dirs = collect_dir_refs(&entries, root);
    ScanResult { entries, dirs, skipped }
}

/// 规范化路径：统一反斜杠、去尾部斜杠，避免 `\\?\` 前缀混入
fn normalize_path(p: &str) -> String {
    let p = p.strip_prefix(r"\\?\").unwrap_or(p);
    let p = p.strip_prefix(r"\\.\").unwrap_or(p);
    p.replace('/', "\\")
}

/// 打开目录句柄读取 64 位 file reference number（NTFS 上即 MFT 引用号，
/// 与 USN 记录中的 FileReferenceNumber 同源）。失败返回 None。
fn dir_file_ref(path: &str) -> Option<u64> {
    let wide = to_wide(path);
    unsafe {
        let handle = CreateFileW(
            PCWSTR(wide.as_ptr()),
            GENERIC_READ.0,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            None,
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS, // 打开目录必须
            None,
        )
        .ok()?;
        let mut info = BY_HANDLE_FILE_INFORMATION::default();
        let ok = GetFileInformationByHandle(handle, &mut info).is_ok();
        let _ = CloseHandle(handle);
        if !ok {
            return None;
        }
        Some(((info.nFileIndexHigh as u64) << 32) | info.nFileIndexLow as u64)
    }
}

/// 为所有目录建立 file_ref -> 路径 映射。
/// 含卷根自身（USN 记录中根目录下文件的 parent_ref 需要它解析）。
fn collect_dir_refs(entries: &[IndexEntry], root: &str) -> HashMap<u64, String> {
    let mut map = HashMap::new();
    if let Some(r) = dir_file_ref(root) {
        map.insert(r, root.to_string());
    }
    for e in entries.iter().filter(|e| e.is_dir) {
        if let Some(r) = dir_file_ref(&e.path) {
            map.insert(r, e.path.clone());
        }
    }
    map
}
