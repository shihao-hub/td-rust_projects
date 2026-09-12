//! 索引数据结构与持久化（bincode 序列化，原子替换写盘）。

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

/// 单条索引条目
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct IndexEntry {
    /// 完整路径，如 `C:\Users\foo\bar.txt`
    pub path: String,
    /// 是否为目录
    pub is_dir: bool,
}

/// 每个卷的 USN Journal 游标状态
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct VolumeState {
    /// 卷根路径，如 `C:\`
    pub root: String,
    /// USN Journal ID；journal 重建后变化，需触发全量重扫
    pub journal_id: u64,
    /// 下次增量读取的起始 USN 位置
    pub next_usn: i64,
}

/// 完整索引
#[derive(Serialize, Deserialize, Default, Debug)]
pub struct Index {
    /// 全部条目（文件 + 目录）
    pub entries: Vec<IndexEntry>,
    /// 目录 file_ref(64 位 MFT 引用号) -> 目录路径。
    /// USN 增量依赖它把「父目录 ref + 文件名」解析为完整路径。
    pub dirs: HashMap<u64, String>,
    /// 各卷 USN 游标
    pub volumes: Vec<VolumeState>,
    /// 上次更新的 unix 时间戳（秒）
    pub updated_at: i64,
    /// 调试用：--root 指定的自定义扫描根（非标准卷枚举时记录）
    pub custom_root: Option<String>,
}

/// 路径 -> entries 下标的辅助索引（不持久化，惰性构建）。
/// 增量 upsert/remove 需要去重定位，避免 O(n) 全扫。
#[derive(Default)]
pub struct PathIndex {
    map: Option<HashMap<String, usize>>,
}

impl PathIndex {
    /// 惰性构建；全量替换 entries 后需调用 invalidate
    fn ensure(&mut self, entries: &[IndexEntry]) {
        if self.map.is_none() {
            let m = entries
                .iter()
                .enumerate()
                .map(|(i, e)| (e.path.clone(), i))
                .collect();
            self.map = Some(m);
        }
    }

    pub fn invalidate(&mut self) {
        self.map = None;
    }

    fn get(&mut self, entries: &[IndexEntry], path: &str) -> Option<usize> {
        self.ensure(entries);
        self.map.as_ref().unwrap().get(path).copied()
    }

    fn insert(&mut self, path: &str, idx: usize) {
        if let Some(m) = &mut self.map {
            m.insert(path.to_string(), idx);
        }
    }

    fn remove(&mut self, path: &str) {
        if let Some(m) = &mut self.map {
            m.remove(path);
        }
    }
}

impl Index {
    /// 索引文件路径：%LOCALAPPDATA%\minieverything\index.bin
    pub fn index_file() -> anyhow::Result<PathBuf> {
        let base = std::env::var("LOCALAPPDATA")
            .or_else(|_| std::env::var("USERPROFILE"))
            .map_err(|_| anyhow::anyhow!("无法定位用户目录"))?;
        Ok(PathBuf::from(base).join("minieverything").join("index.bin"))
    }

    /// 从磁盘加载索引；不存在返回 None
    pub fn load() -> anyhow::Result<Option<Index>> {
        let path = Self::index_file()?;
        if !path.exists() {
            return Ok(None);
        }
        let bytes = std::fs::read(&path)?;
        let idx = bincode::deserialize(&bytes)
            .map_err(|e| anyhow::anyhow!("索引文件损坏（{path:?}）：{e}；请运行 update --rebuild 重建"))?;
        Ok(Some(idx))
    }

    /// 原子保存：先写 .tmp 再 rename 覆盖
    pub fn save(&self) -> anyhow::Result<()> {
        let path = Self::index_file()?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let bytes = bincode::serialize(self)?;
        let tmp = path.with_extension("bin.tmp");
        std::fs::write(&tmp, &bytes)?;
        std::fs::rename(&tmp, &path)?;
        Ok(())
    }

    /// 新建索引（全量扫描结果）
    pub fn from_scan(
        entries: Vec<IndexEntry>,
        dirs: HashMap<u64, String>,
        volumes: Vec<VolumeState>,
        custom_root: Option<String>,
    ) -> Self {
        Self {
            entries,
            dirs,
            volumes,
            updated_at: chrono::Local::now().timestamp(),
            custom_root,
        }
    }

    /// 用新数据整体替换某个卷（或自定义根前缀）下的条目与目录映射
    pub fn replace_prefix(
        &mut self,
        prefix: &str,
        entries_new: Vec<IndexEntry>,
        dirs_new: HashMap<u64, String>,
        path_index: &mut PathIndex,
    ) {
        self.entries.retain(|e| !e.path.starts_with(prefix));
        self.entries.extend(entries_new);
        self.dirs.retain(|_, p| !p.starts_with(prefix));
        self.dirs.extend(dirs_new);
        path_index.invalidate();
    }

    /// 插入或更新一条路径（增量 CREATE / RENAME_NEW 用）
    pub fn upsert_path(&mut self, path: &str, is_dir: bool, path_index: &mut PathIndex) {
        if let Some(i) = path_index.get(&self.entries, path) {
            self.entries[i].is_dir = is_dir;
            return;
        }
        self.entries.push(IndexEntry { path: path.to_string(), is_dir });
        path_index.insert(path, self.entries.len() - 1);
    }

    /// 删除一条路径（增量 DELETE / RENAME_OLD 用）
    pub fn remove_path(&mut self, path: &str, path_index: &mut PathIndex) {
        if let Some(i) = path_index.get(&self.entries, path) {
            // 与末尾交换后 pop，O(1) 删除；同步修正被换元素的下标
            let last = self.entries.len() - 1;
            self.entries.swap(i, last);
            let moved = self.entries.pop().unwrap();
            if i < self.entries.len() {
                if let Some(m) = &mut path_index.map {
                    m.insert(moved.path.clone(), i);
                }
            }
            path_index.remove(path);
        }
    }

    /// 目录改名/移动：把 old 路径（及其子树）批量替换为 new 路径。
    /// 按路径段边界匹配，避免 `C:\a` 误伤 `C:\ab` 这类前缀碰撞。
    pub fn rename_prefix(&mut self, old: &str, new: &str, path_index: &mut PathIndex) {
        let seg = |p: &str| -> Option<String> {
            if p == old {
                Some(new.to_string())
            } else {
                let prefix = format!("{old}\\");
                p.strip_prefix(&prefix).map(|rest| format!("{new}\\{rest}"))
            }
        };
        for e in &mut self.entries {
            if let Some(np) = seg(&e.path) {
                e.path = np;
            }
        }
        for p in self.dirs.values_mut() {
            if let Some(np) = seg(p) {
                *p = np;
            }
        }
        path_index.invalidate();
    }

    /// 查询该卷游标状态；不存在返回 None
    pub fn volume_state(&self, root: &str) -> Option<&VolumeState> {
        self.volumes.iter().find(|v| v.root.eq_ignore_ascii_case(root))
    }

    /// 更新（或插入）该卷游标状态
    pub fn set_volume_state(&mut self, state: VolumeState) {
        if let Some(v) = self
            .volumes
            .iter_mut()
            .find(|v| v.root.eq_ignore_ascii_case(&state.root))
        {
            *v = state;
        } else {
            self.volumes.push(state);
        }
    }

    /// 条目统计（文件数， 目录数）
    pub fn stats(&self) -> (usize, usize) {
        let files = self.entries.iter().filter(|e| !e.is_dir).count();
        let dirs = self.entries.len() - files;
        (files, dirs)
    }
}

/// 取路径的文件名部分（最后一个 `\` 之后）
pub fn basename(path: &str) -> &str {
    match path.rfind('\\') {
        Some(i) => &path[i + 1..],
        None => path,
    }
}
