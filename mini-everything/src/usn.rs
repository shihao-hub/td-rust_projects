//! USN Journal 增量更新：从上次游标位置读取变更日志并应用到索引。
//!
//! 设计要点：
//! - USN 记录只含 file_ref / parent_ref / 文件名，不含完整路径；
//!   依赖索引中的 `dirs: HashMap<file_ref, path>` 解析父目录路径。
//! - 目录改名/移动时 USN 只为目录本身生成 RENAME 记录，
//!   子项路径通过 `rename_prefix` 批量前缀替换修正。
//! - journal_id 变化或游标落后于 LowestValidUsn（环形缓冲已覆盖）时，
//!   该卷触发全量重扫兜底。

use crate::index::{Index, PathIndex, VolumeState};
use crate::scan;
use crate::volume::{to_wide, volume_device_path};
use windows::core::PCWSTR;
use windows::Win32::Foundation::{CloseHandle, GENERIC_READ, HANDLE};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows::Win32::System::IO::DeviceIoControl;
use windows::Win32::System::Ioctl::{
    FSCTL_CREATE_USN_JOURNAL, FSCTL_QUERY_USN_JOURNAL, FSCTL_READ_USN_JOURNAL,
};

/// win32 错误码：USN Journal 不存在
const ERROR_JOURNAL_NOT_EXIST: i32 = 1178;

/// USN_REASON_* 关键位（完整定义见 winioctl.h）
const REASON_FILE_CREATE: u32 = 0x0000_0100;
const REASON_FILE_DELETE: u32 = 0x0000_0200;
const REASON_RENAME_OLD_NAME: u32 = 0x0000_1000;
const REASON_RENAME_NEW_NAME: u32 = 0x0000_2000;
/// FILE_ATTRIBUTE_DIRECTORY
const ATTR_DIRECTORY: u32 = 0x0000_0010;

/// 卷的 USN Journal 查询结果（手解 USN_JOURNAL_DATA_V0 布局，48 字节）
#[derive(Debug, Clone)]
pub struct JournalInfo {
    pub journal_id: u64,
    #[allow(dead_code)] // 调试时观察环形缓冲起点
    pub first_usn: i64,
    pub next_usn: i64,
    pub lowest_valid_usn: i64,
}

/// 单次卷增量的结果
pub enum IncrementalOutcome {
    /// 成功应用 N 条记录
    Applied(usize),
    /// 无法使用 USN（无权限 / 无 journal），原因见字符串
    Unavailable(String),
    /// 数据不一致，已对该卷全量重扫
    Rescanned(String),
}

/// 打开卷设备（如 `\\.\C:`）。需要管理员权限。
fn open_volume(root: &str) -> anyhow::Result<HANDLE> {
    let dev = volume_device_path(root);
    let wide = to_wide(&dev);
    unsafe {
        CreateFileW(
            PCWSTR(wide.as_ptr()),
            GENERIC_READ.0,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            None,
            OPEN_EXISTING,
            Default::default(),
            None,
        )
        .map_err(|e| anyhow::anyhow!("打开卷 {dev} 失败：{e}"))
    }
}

/// FSCTL_QUERY_USN_JOURNAL（返回原始 windows 错误，便于按错误码分支）
fn query_journal_raw(handle: HANDLE) -> Result<JournalInfo, windows::core::Error> {
    let mut out = windows::Win32::System::Ioctl::USN_JOURNAL_DATA_V1::default();
    let mut returned = 0u32;
    unsafe {
        DeviceIoControl(
            handle,
            FSCTL_QUERY_USN_JOURNAL,
            None,
            0,
            Some(&mut out as *mut _ as _),
            std::mem::size_of_val(&out) as u32,
            Some(&mut returned),
            None,
        )?;
    }
    if (returned as usize) < std::mem::size_of::<windows::Win32::System::Ioctl::USN_JOURNAL_DATA_V0>() {
        return Err(windows::core::Error::from_win32());
    }
    Ok(JournalInfo {
        journal_id: out.UsnJournalID,
        first_usn: out.FirstUsn,
        next_usn: out.NextUsn,
        lowest_valid_usn: out.LowestValidUsn,
    })
}

/// FSCTL_QUERY_USN_JOURNAL（anyhow 封装）
fn query_journal(handle: HANDLE) -> anyhow::Result<JournalInfo> {
    query_journal_raw(handle).map_err(|e| anyhow::anyhow!("查询 USN Journal 失败：{e}"))
}

/// 手工构造 READ_USN_JOURNAL_DATA_V0 输入（40 字节）：
/// StartUsn(i64) ReasonMask(u32) ReturnOnlyOnClose(u32)
/// Timeout(u64) BytesToWaitFor(u64) UsnJournalID(u64)
/// 注意：UsnJournalID 必须携带，驱动用它检测 journal 重建竞态
fn build_read_input(start_usn: i64, journal_id: u64) -> [u8; 40] {
    let mut b = [0u8; 40];
    b[0..8].copy_from_slice(&start_usn.to_le_bytes());
    b[8..12].copy_from_slice(&0xFFFF_FFFFu32.to_le_bytes()); // ReasonMask: 全部
    b[12..16].copy_from_slice(&0u32.to_le_bytes()); // ReturnOnlyOnClose
    b[16..24].copy_from_slice(&0u64.to_le_bytes()); // Timeout: 不等待
    b[24..32].copy_from_slice(&0u64.to_le_bytes()); // BytesToWaitFor: 立即返回
    b[32..40].copy_from_slice(&journal_id.to_le_bytes()); // UsnJournalID
    b
}

/// 一条解析后的 USN_RECORD_V2
struct UsnRecord {
    file_ref: u64,
    parent_ref: u64,
    reason: u32,
    file_attrs: u32,
    name: String,
}

/// 从缓冲区解析全部 USN 记录（跳过开头 8 字节 NextUsn）。
/// 文件名起始位置以记录内 FileNameOffset 字段为准（V2 通常为 60）。
fn parse_records(buf: &[u8]) -> Vec<UsnRecord> {
    let mut out = Vec::new();
    let mut off = 8usize;
    while off + 60 <= buf.len() {
        let rec_len = u32::from_le_bytes(buf[off..off + 4].try_into().unwrap()) as usize;
        if rec_len < 60 || off + rec_len > buf.len() {
            break;
        }
        let file_ref = u64::from_le_bytes(buf[off + 8..off + 16].try_into().unwrap());
        let parent_ref = u64::from_le_bytes(buf[off + 16..off + 24].try_into().unwrap());
        let reason = u32::from_le_bytes(buf[off + 40..off + 44].try_into().unwrap());
        let file_attrs = u32::from_le_bytes(buf[off + 52..off + 56].try_into().unwrap());
        let name_len = u16::from_le_bytes(buf[off + 56..off + 58].try_into().unwrap()) as usize;
        let name_off = u16::from_le_bytes(buf[off + 58..off + 60].try_into().unwrap()) as usize;
        if name_off < 60 || name_off + name_len > rec_len {
            break;
        }
        let name_units: Vec<u16> = buf[off + name_off..off + name_off + name_len]
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        out.push(UsnRecord {
            file_ref,
            parent_ref,
            reason,
            file_attrs,
            name: String::from_utf16_lossy(&name_units),
        });
        off += rec_len;
    }
    out
}

/// 读 USN Journal 直到追平，返回（最终 NextUsn 游标, 记录数）
fn read_records(handle: HANDLE, start_usn: i64, journal_id: u64) -> anyhow::Result<(i64, Vec<UsnRecord>)> {
    let mut all = Vec::new();
    let mut cursor = start_usn;
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let input = build_read_input(cursor, journal_id);
        let mut returned = 0u32;
        let result = unsafe {
            DeviceIoControl(
                handle,
                FSCTL_READ_USN_JOURNAL,
                Some(input.as_ptr() as _),
                input.len() as u32,
                Some(buf.as_mut_ptr() as _),
                buf.len() as u32,
                Some(&mut returned),
                None,
            )
        };
        if let Err(err) = result {
            let io_err = std::io::Error::from(err);
            // 追平后再读可能返回 ERROR_HANDLE_EOF (38)
            if raw_os_error(&io_err) == Some(38) {
                break;
            }
            return Err(anyhow::anyhow!("读取 USN Journal 失败：{io_err}"));
        }
        let returned = returned as usize;
        if returned < 8 {
            break;
        }
        let next = i64::from_le_bytes(buf[0..8].try_into().unwrap());
        let records = parse_records(&buf[..returned]);
        let got = !records.is_empty();
        all.extend(records);
        if !got || next <= cursor {
            cursor = next.max(cursor);
            break;
        }
        cursor = next;
    }
    Ok((cursor, all))
}

fn raw_os_error(e: &std::io::Error) -> Option<i32> {
    e.raw_os_error()
}

/// 查询某卷当前 journal 状态并封装为 VolumeState（全量构建后记录游标用）。
/// 无权限 / 无 journal 时返回 Err。
pub fn query_journal_state(root: &str) -> anyhow::Result<crate::index::VolumeState> {
    let info = ensure_journal(root)?;
    Ok(crate::index::VolumeState {
        root: root.to_string(),
        journal_id: info.journal_id,
        next_usn: info.next_usn,
    })
}

/// 确保 journal 存在（不存在则按系统默认尺寸创建，需管理员），返回其状态。
/// 全量扫描前调用：先建 journal 再扫描，扫描期间的变更留待下次增量幂等补上。
pub fn ensure_journal(root: &str) -> anyhow::Result<JournalInfo> {
    let handle = open_volume(root)?;
    let result = (|| -> anyhow::Result<JournalInfo> {
        if let Err(e) = query_journal_raw(handle) {
            let code = e.code().0;
            // 0x8007xxxx 形式的 HRESULT，低 16 位即 win32 错误码
            let win32 = (code & 0xFFFF) as u32;
            if win32 as i32 != ERROR_JOURNAL_NOT_EXIST {
                return Err(anyhow::anyhow!("查询 USN Journal 失败：{e}"));
            }
        } else {
            return query_journal(handle);
        }
        // 创建 journal：MaximumSize=0 / AllocationDelta=0 表示系统默认（约 32MB）
        let input = [0u8; 16];
        let mut returned = 0u32;
        unsafe {
            DeviceIoControl(
                handle,
                FSCTL_CREATE_USN_JOURNAL,
                Some(input.as_ptr() as _),
                input.len() as u32,
                None,
                0,
                Some(&mut returned),
                None,
            )
            .map_err(|e| anyhow::anyhow!("创建 USN Journal 失败：{e}"))?;
        }
        query_journal(handle)
    })();
    let _ = unsafe { CloseHandle(handle) };
    result
}

/// 对单个卷执行增量：成功则更新索引与游标。
/// `allow_rescan` 为 false 时（搜索前刷新场景）不触发全量重扫，仅返回提示。
pub fn incremental_update_volume(
    index: &mut Index,
    root: &str,
    path_index: &mut PathIndex,
    allow_rescan: bool,
) -> IncrementalOutcome {
    // 1. 打开卷并查询 journal 状态
    let handle = match open_volume(root) {
        Ok(h) => h,
        Err(e) => return IncrementalOutcome::Unavailable(e.to_string()),
    };
    let result = (|| -> anyhow::Result<IncrementalOutcome> {
        let info = query_journal(handle)?;
        let saved = index.volume_state(root).cloned();

        // 2. 一致性校验
        match &saved {
            // 索引建立时无管理员权限、未记录游标：
            // 采纳当前 NextUsn 作为游标（不回放历史，避免重复应用），
            // 建索引至今的小窗口变更可能缺失，精确同步需 --rebuild
            None => {
                index.set_volume_state(VolumeState {
                    root: root.to_string(),
                    journal_id: info.journal_id,
                    next_usn: info.next_usn,
                });
                eprintln!("提示：{root} 首次记录 USN 游标，之前的少量变更可能未入库（精确同步请 update --rebuild）");
                return Ok(IncrementalOutcome::Applied(0));
            }
            Some(vs) if vs.journal_id == 0 => {
                // 旧版/降级路径记录的空游标，同上采纳
                index.set_volume_state(VolumeState {
                    root: root.to_string(),
                    journal_id: info.journal_id,
                    next_usn: info.next_usn,
                });
                eprintln!("提示：{root} 首次记录 USN 游标，之前的少量变更可能未入库（精确同步请 update --rebuild）");
                return Ok(IncrementalOutcome::Applied(0));
            }
            Some(vs) if vs.journal_id != info.journal_id => {
                let reason = "USN Journal 已重建（journal_id 变化）".to_string();
                if allow_rescan {
                    return Ok(IncrementalOutcome::Rescanned(reason));
                }
                return Ok(IncrementalOutcome::Unavailable(format!("{reason}，需运行 update 全量重扫")));
            }
            Some(vs) if vs.next_usn < info.lowest_valid_usn => {
                let reason = "USN Journal 已被覆盖（游标过期）".to_string();
                if allow_rescan {
                    return Ok(IncrementalOutcome::Rescanned(reason));
                }
                return Ok(IncrementalOutcome::Unavailable(format!("{reason}，需运行 update 全量重扫")));
            }
            Some(_) => {}
        }

        // 3. 读取并应用增量记录
        let start_usn = saved.as_ref().unwrap().next_usn;
        let (final_usn, records) = read_records(handle, start_usn, info.journal_id)?;
        let mut applied = 0usize;
        // 两条记录式改名的衔接：RENAME_OLD 记录旧路径，RENAME_NEW 记录新路径，
        // 中间靠 file_ref 暂存旧路径
        let mut pending_renames: std::collections::HashMap<u64, String> =
            std::collections::HashMap::new();
        for rec in &records {
            if apply_record(index, rec, path_index, &mut pending_renames) {
                applied += 1;
            }
        }

        // 4. 推进游标并保存
        index.set_volume_state(VolumeState {
            root: root.to_string(),
            journal_id: info.journal_id,
            next_usn: final_usn.max(start_usn),
        });
        Ok(IncrementalOutcome::Applied(applied))
    })();

    let _ = unsafe { CloseHandle(handle) };

    match result {
        Ok(o @ IncrementalOutcome::Applied(_)) => o,
        Ok(IncrementalOutcome::Unavailable(m)) => IncrementalOutcome::Unavailable(m),
        Ok(IncrementalOutcome::Rescanned(reason)) => {
            full_rescan_volume(index, root, path_index);
            IncrementalOutcome::Rescanned(reason)
        }
        Err(e) => IncrementalOutcome::Unavailable(e.to_string()),
    }
}

/// 应用一条 USN 记录到索引。
/// 返回 false 表示记录与路径无关（纯数据变更、关闭等），无需计数。
///
/// 记录语义（按位组合判断）：
/// - DELETE                    -> 删除路径；目录需连同子树清理
/// - RENAME_OLD_NAME（单独）    -> 移走旧路径：仅移除该条目，子树待 NEW 记录到达后前缀重写
/// - RENAME_NEW_NAME（单独）    -> 落位新路径：与 OLD 半程（或 pending）配对做前缀重写
/// - RENAME_OLD|RENAME_NEW 合并 -> 单条记录即改名：parent/name 即新位置
/// - CREATE                    -> 新增路径
fn apply_record(
    index: &mut Index,
    rec: &UsnRecord,
    path_index: &mut PathIndex,
    pending_renames: &mut std::collections::HashMap<u64, String>,
) -> bool {
    let deleted = rec.reason & REASON_FILE_DELETE != 0;
    let ren_old = rec.reason & REASON_RENAME_OLD_NAME != 0;
    let ren_new = rec.reason & REASON_RENAME_NEW_NAME != 0;
    let created = rec.reason & REASON_FILE_CREATE != 0;
    if !deleted && !ren_old && !ren_new && !created {
        return false; // 路径未变化（数据写入、属性变更、关闭等）
    }

    let parent_path = index.dirs.get(&rec.parent_ref).map(|p| {
        p.trim_end_matches('\\').to_string()
    });
    let join = |parent: &str, name: &str| format!("{}\\{}", parent.trim_end_matches('\\'), name);
    let dir_known = index.dirs.get(&rec.file_ref).cloned();

    // ---- 1) 消失/移走：DELETE 与 RENAME_OLD ----
    if deleted {
        // 目录删除：USN 不会为子项逐条发记录，子树一并清理
        match &dir_known {
            Some(old) => {
                let sub = format!("{}\\", old.trim_end_matches('\\'));
                index.entries.retain(|e| !e.path.starts_with(&sub));
                index.dirs.retain(|_, p| !p.starts_with(&sub));
                index.remove_path(old, path_index);
                index.dirs.remove(&rec.file_ref);
                path_index.invalidate();
            }
            None => {
                // 文件删除：DELETE 记录中 parent+name 即真实最终路径
                if let Some(p) = &parent_path {
                    index.remove_path(&join(p, &rec.name), path_index);
                }
            }
        }
        pending_renames.remove(&rec.file_ref);
        return true;
    }

    if ren_old && !ren_new {
        // 两条记录式改名的上半程：
        // - 文件：移除旧条目，等 NEW 记录落位
        // - 目录：条目与子树保持原样（仅记 pending），待 NEW 记录到达后整体前缀重写
        let old = dir_known
            .clone()
            .or_else(|| parent_path.clone().map(|p| join(&p, &rec.name)));
        if let Some(old) = old {
            if dir_known.is_none() {
                index.remove_path(&old, path_index);
            }
            pending_renames.insert(rec.file_ref, old);
        }
        // 若同一条还带 CREATE（先删后建等罕见场景），交由下方统一落位
    }

    // ---- 2) 出现/落位：CREATE 与 RENAME_NEW ----
    if created || ren_new {
        if let Some(parent) = &parent_path {
            let new_path = join(parent, &rec.name);
            let is_dir = rec.file_attrs & ATTR_DIRECTORY != 0;

            // 改名落位：优先 pending（两条式）、其次 dirs（合并式），拿旧路径做前缀重写
            let old_path = pending_renames
                .remove(&rec.file_ref)
                .or_else(|| dir_known.clone().filter(|_| ren_old && ren_new));
            if let Some(old) = old_path {
                if is_dir && !old.eq_ignore_ascii_case(&new_path) {
                    // 目录改名/移动：子树路径批量前缀替换，并确保目录自身条目在位
                    index.rename_prefix(&old, &new_path, path_index);
                    index.upsert_path(&new_path, true, path_index);
                    index.dirs.insert(rec.file_ref, new_path.clone());
                } else if !old.eq_ignore_ascii_case(&new_path) {
                    // 文件改名：删旧增新
                    index.remove_path(&old, path_index);
                    index.upsert_path(&new_path, is_dir, path_index);
                } else {
                    // 大小写修正类改名：路径不变，仅刷条目
                    index.upsert_path(&new_path, is_dir, path_index);
                }
                return true;
            }

            // 普通新增
            index.upsert_path(&new_path, is_dir, path_index);
            if is_dir {
                index.dirs.insert(rec.file_ref, new_path);
            } else {
                index.dirs.remove(&rec.file_ref);
            }
        }
        // parent 未知：无法定位，保守忽略（下次全量重扫兜底）
    }

    true
}

/// 对单个卷全量重扫并替换该卷数据
fn full_rescan_volume(index: &mut Index, root: &str, path_index: &mut PathIndex) {
    let r = scan::scan_root(root);
    let prefix = root.trim_end_matches('\\').to_string() + "\\";
    index.replace_prefix(&prefix, r.entries, r.dirs, path_index);
    // 重扫后重新记录游标
    if let Ok(h) = open_volume(root) {
        if let Ok(info) = query_journal(h) {
            index.set_volume_state(VolumeState {
                root: root.to_string(),
                journal_id: info.journal_id,
                next_usn: info.next_usn,
            });
        }
        let _ = unsafe { CloseHandle(h) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::{Index, IndexEntry, PathIndex};

    /// 构造测试索引：C:\ 下有 a\（含 f.txt）、b\ 两个目录
    fn test_index() -> (Index, PathIndex) {
        let mut idx = Index::default();
        for (p, d) in [
            (r"C:\a", true),
            (r"C:\a\f.txt", false),
            (r"C:\b", true),
        ] {
            idx.entries.push(IndexEntry { path: p.into(), is_dir: d });
        }
        // 目录 ref：根 C:\=50, a=100, b=200
        idx.dirs.insert(50, r"C:\".into());
        idx.dirs.insert(100, r"C:\a".into());
        idx.dirs.insert(200, r"C:\b".into());
        (idx, PathIndex::default())
    }

    fn rec(file_ref: u64, parent_ref: u64, reason: u32, attrs: u32, name: &str) -> UsnRecord {
        UsnRecord { file_ref, parent_ref, reason, file_attrs: attrs, name: name.into() }
    }

    fn paths(idx: &Index) -> Vec<&str> {
        idx.entries.iter().map(|e| e.path.as_str()).collect()
    }

    #[test]
    fn file_create_and_delete() {
        let (mut idx, mut pi) = test_index();
        let mut pend = Default::default();
        // 新建文件 C:\a\new.txt
        assert!(apply_record(&mut idx, &rec(300, 100, REASON_FILE_CREATE, 0, "new.txt"), &mut pi, &mut pend));
        assert!(paths(&idx).contains(&r"C:\a\new.txt"));
        // 删除文件
        assert!(apply_record(&mut idx, &rec(300, 100, REASON_FILE_DELETE, 0, "new.txt"), &mut pi, &mut pend));
        assert!(!paths(&idx).contains(&r"C:\a\new.txt"));
        assert_eq!(idx.entries.len(), 3);
    }

    #[test]
    fn file_rename_two_records() {
        let (mut idx, mut pi) = test_index();
        let mut pend = Default::default();
        // f.txt -> g.txt（两条记录式）
        apply_record(&mut idx, &rec(500, 100, REASON_RENAME_OLD_NAME, 0, "f.txt"), &mut pi, &mut pend);
        assert!(!paths(&idx).contains(&r"C:\a\f.txt"));
        apply_record(&mut idx, &rec(500, 100, REASON_RENAME_NEW_NAME, 0, "g.txt"), &mut pi, &mut pend);
        assert!(paths(&idx).contains(&r"C:\a\g.txt"));
        assert_eq!(idx.entries.len(), 3);
    }

    #[test]
    fn file_move_between_dirs() {
        let (mut idx, mut pi) = test_index();
        let mut pend = Default::default();
        apply_record(&mut idx, &rec(500, 100, REASON_RENAME_OLD_NAME, 0, "f.txt"), &mut pi, &mut pend);
        apply_record(&mut idx, &rec(500, 200, REASON_RENAME_NEW_NAME, 0, "f.txt"), &mut pi, &mut pend);
        assert!(paths(&idx).contains(&r"C:\b\f.txt"));
        assert!(!paths(&idx).contains(&r"C:\a\f.txt"));
    }

    #[test]
    fn dir_rename_moves_subtree() {
        let (mut idx, mut pi) = test_index();
        let mut pend = Default::default();
        // a -> aa（两条记录式）：子文件 f.txt 应跟随为 C:\aa\f.txt
        apply_record(&mut idx, &rec(100, 50, REASON_RENAME_OLD_NAME, ATTR_DIRECTORY, "a"), &mut pi, &mut pend);
        assert_eq!(idx.dirs.get(&100).map(String::as_str), Some(r"C:\a"));
        apply_record(&mut idx, &rec(100, 50, REASON_RENAME_NEW_NAME, ATTR_DIRECTORY, "aa"), &mut pi, &mut pend);
        assert!(paths(&idx).contains(&r"C:\aa\f.txt"));
        assert!(!paths(&idx).contains(&r"C:\a\f.txt"));
        assert_eq!(idx.dirs.get(&100).map(String::as_str), Some(r"C:\aa"));
        assert_eq!(idx.entries.len(), 3);
    }

    #[test]
    fn dir_rename_combined_record() {
        let (mut idx, mut pi) = test_index();
        let mut pend = Default::default();
        // 合并式单条记录：parent/name 即新位置
        apply_record(&mut idx, &rec(100, 50, REASON_RENAME_OLD_NAME | REASON_RENAME_NEW_NAME, ATTR_DIRECTORY, "ab"), &mut pi, &mut pend);
        assert!(paths(&idx).contains(&r"C:\ab\f.txt"));
        assert_eq!(idx.dirs.get(&100).map(String::as_str), Some(r"C:\ab"));
    }

    #[test]
    fn rename_prefix_collision_safe() {
        // 存在兄弟目录 C:\ab 时把 C:\a 改名为 C:\aa，不得误伤 C:\ab 子树
        let (mut idx, mut pi) = test_index();
        idx.entries.push(IndexEntry { path: r"C:\ab\z.txt".into(), is_dir: false });
        idx.entries.push(IndexEntry { path: r"C:\ab".into(), is_dir: true });
        idx.dirs.insert(400, r"C:\ab".into());
        let mut pend = Default::default();
        apply_record(&mut idx, &rec(100, 50, REASON_RENAME_OLD_NAME, ATTR_DIRECTORY, "a"), &mut pi, &mut pend);
        apply_record(&mut idx, &rec(100, 50, REASON_RENAME_NEW_NAME, ATTR_DIRECTORY, "aa"), &mut pi, &mut pend);
        assert!(paths(&idx).contains(&r"C:\aa\f.txt"));
        assert!(paths(&idx).contains(&r"C:\ab\z.txt"));
        assert_eq!(idx.dirs.get(&400).map(String::as_str), Some(r"C:\ab"));
        assert_eq!(idx.dirs.get(&100).map(String::as_str), Some(r"C:\aa"));
        assert_eq!(idx.entries.len(), 5);
    }

    #[test]
    fn dir_delete_removes_subtree() {
        let (mut idx, mut pi) = test_index();
        let mut pend = Default::default();
        apply_record(&mut idx, &rec(100, 50, REASON_FILE_DELETE, ATTR_DIRECTORY, "a"), &mut pi, &mut pend);
        assert!(!paths(&idx).contains(&r"C:\a"));
        assert!(!paths(&idx).contains(&r"C:\a\f.txt"));
        assert_eq!(idx.entries.len(), 1);
        assert!(!idx.dirs.contains_key(&100));
    }

    #[test]
    fn new_dir_and_file_inside() {
        let (mut idx, mut pi) = test_index();
        let mut pend = Default::default();
        // 新建目录 C:\c（ref=300），再在其中新建文件
        apply_record(&mut idx, &rec(300, 50, REASON_FILE_CREATE, ATTR_DIRECTORY, "c"), &mut pi, &mut pend);
        assert!(paths(&idx).contains(&r"C:\c"));
        apply_record(&mut idx, &rec(301, 300, REASON_FILE_CREATE, 0, "x.txt"), &mut pi, &mut pend);
        assert!(paths(&idx).contains(&r"C:\c\x.txt"));
    }

    #[test]
    fn irrelevant_reason_ignored() {
        let (mut idx, mut pi) = test_index();
        let mut pend = Default::default();
        // 纯数据变更（0x4 = DATA_EXTEND）+ 关闭（0x80000000）
        assert!(!apply_record(&mut idx, &rec(500, 100, 0x4 | 0x8000_0000, 0, "f.txt"), &mut pi, &mut pend));
        assert_eq!(idx.entries.len(), 3);
    }
}
