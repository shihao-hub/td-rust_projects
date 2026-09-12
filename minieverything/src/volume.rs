//! 卷枚举：找出所有固定磁盘上的 NTFS 卷（USN Journal 仅 NTFS 支持）。

use windows::core::PCWSTR;
use windows::Win32::Storage::FileSystem::{
    GetDriveTypeW, GetLogicalDrives, GetVolumeInformationW,
};
use windows::Win32::System::WindowsProgramming::DRIVE_FIXED;

/// 一个待索引的 NTFS 卷
#[derive(Debug, Clone)]
pub struct Volume {
    /// 卷根路径，形如 `C:\`
    pub root: String,
    /// 盘符字母，如 'C'
    #[allow(dead_code)] // status 输出预留
    pub letter: char,
    /// 文件系统名，如 "NTFS"
    #[allow(dead_code)] // status 输出预留
    pub fs: String,
}

/// 字符串转 UTF-16 宽字符串（带 NUL 终止）
pub fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// 枚举所有固定磁盘（DRIVE_FIXED）且文件系统为 NTFS 的卷。
/// 失败的盘符（无法查询卷信息）直接跳过。
pub fn enumerate_ntfs_volumes() -> anyhow::Result<Vec<Volume>> {
    let mask = unsafe { GetLogicalDrives() };
    if mask == 0 {
        anyhow::bail!("GetLogicalDrives 失败：{}", std::io::Error::last_os_error());
    }

    let mut volumes = Vec::new();
    for i in 0..26u32 {
        if mask & (1 << i) == 0 {
            continue;
        }
        let letter = char::from_u32(u32::from(b'A') + i).unwrap();
        let root = format!("{letter}:\\");
        let root_w = to_wide(&root);

        // 只索引固定磁盘（排除 U 盘 / 光驱 / 网络盘）
        if unsafe { GetDriveTypeW(PCWSTR(root_w.as_ptr())) } != DRIVE_FIXED {
            continue;
        }

        // 查询文件系统名，只接受 NTFS（USN Journal 依赖）
        let mut fs_name = [0u16; 32];
        let ok = unsafe {
            GetVolumeInformationW(
                PCWSTR(root_w.as_ptr()),
                None,
                None,
                None,
                None,
                Some(&mut fs_name),
            )
        }
        .is_ok();
        if !ok {
            eprintln!("警告：无法读取卷信息，跳过 {root}");
            continue;
        }
        let fs = String::from_utf16_lossy(
            &fs_name[..fs_name.iter().position(|&c| c == 0).unwrap_or(fs_name.len())],
        );
        if !fs.eq_ignore_ascii_case("NTFS") {
            eprintln!("提示：{root} 为 {fs}（非 NTFS），跳过");
            continue;
        }

        volumes.push(Volume { root, letter, fs });
    }
    Ok(volumes)
}

/// 从卷根路径推导卷设备路径：`C:\` -> `\\.\C:`
pub fn volume_device_path(root: &str) -> String {
    let letter = root.chars().next().unwrap_or('C');
    format!(r"\\.\{letter}:")
}
