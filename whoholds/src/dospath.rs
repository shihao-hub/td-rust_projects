//! \Device\... 设备路径 → DOS/UNC 路径映射。
//! 卷设备通过 QueryDosDeviceW 反查盘符；网络重定向器路径按固定格式转换，
//! 不使用 WNetGetUniversalNameW（对失效网络路径可能阻塞）。
use crate::winapi;

pub(crate) struct DosPathMapper {
    /// (小写设备前缀, DOS 前缀)，按前缀长度降序排列，避免 HarddiskVolume1 误配 HarddiskVolume10
    pairs: Vec<(String, String)>,
}

impl DosPathMapper {
    pub(crate) fn system() -> Self {
        let mut pairs = Vec::new();
        let mut drives = [0u16; 512];
        let n = unsafe { winapi::GetLogicalDriveStringsW(drives.len() as u32, drives.as_mut_ptr()) }
            as usize;
        if n == 0 || n > drives.len() {
            return Self::from_pairs(pairs);
        }
        let mut i = 0usize;
        while i < n {
            let Some(rel) = drives[i..n].iter().position(|&c| c == 0) else {
                break;
            };
            let drive = String::from_utf16_lossy(&drives[i..i + rel]); // "C:\"
            i += rel + 1;
            if drive.len() < 2 {
                continue;
            }
            let dev_name = winapi::to_wide(&drive[..2]); // "C:"
            let mut target = [0u16; 1024];
            let t = unsafe {
                winapi::QueryDosDeviceW(dev_name.as_ptr(), target.as_mut_ptr(), target.len() as u32)
            };
            if t > 0 {
                let dev = winapi::from_wide(&target[..t as usize]);
                if dev.starts_with("\\Device\\") {
                    pairs.push((dev, drive[..2].to_string())); // "\Device\HarddiskVolume3" → "C:"
                }
            }
        }
        Self::from_pairs(pairs)
    }

    pub(crate) fn from_pairs(pairs: Vec<(String, String)>) -> Self {
        let mut pairs: Vec<(String, String)> = pairs
            .into_iter()
            .map(|(d, p)| (d.to_lowercase(), p))
            .collect();
        pairs.sort_by(|a, b| b.0.len().cmp(&a.0.len()));
        Self { pairs }
    }

    /// 设备路径转 DOS/UNC；转不了返回 None（调用方原样输出）
    pub(crate) fn map(&self, name: &str) -> Option<String> {
        let lower = name.to_lowercase();
        for (dev, dos) in &self.pairs {
            if lower.starts_with(dev.as_str()) {
                return Some(format!("{dos}{}", &name[dev.len()..]));
            }
        }
        const LANMAN: &str = "\\device\\lanmanredirector";
        if lower.starts_with(LANMAN) {
            // \Device\LanmanRedirector\;C:0000000000000000\server\share → \\server\share
            // 去掉会话组件 ";C:0000..." 后整体作为 UNC
            let rest = name[LANMAN.len()..].trim_start_matches('\\');
            return rest.find('\\').map(|p| format!("\\\\{}", &rest[p + 1..]));
        }
        const MUP: &str = "\\device\\mup";
        if lower.starts_with(MUP) {
            let rest = name[MUP.len()..].trim_start_matches('\\');
            if rest.is_empty() {
                return None;
            }
            return Some(format!("\\\\{rest}"));
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn volume_mapping_prefers_longer_prefix() {
        let m = DosPathMapper::from_pairs(vec![
            ("\\Device\\HarddiskVolume1".into(), "C:".into()),
            ("\\Device\\HarddiskVolume10".into(), "X:".into()),
        ]);
        assert_eq!(
            m.map("\\Device\\HarddiskVolume10\\a\\b.txt").unwrap(),
            "X:\\a\\b.txt"
        );
        assert_eq!(m.map("\\Device\\HarddiskVolume1\\a").unwrap(), "C:\\a");
        assert!(m.map("\\Device\\NamedPipe\\x").is_none());
    }

    #[test]
    fn unc_mapping() {
        let m = DosPathMapper::from_pairs(vec![]);
        assert_eq!(
            m.map("\\Device\\LanmanRedirector\\;C:0000000000000000\\srv\\share\\f.txt")
                .unwrap(),
            "\\\\srv\\share\\f.txt"
        );
        assert_eq!(
            m.map("\\Device\\Mup\\srv\\share").unwrap(),
            "\\\\srv\\share"
        );
        assert!(m.map("\\Device\\Mup").is_none());
    }
}
