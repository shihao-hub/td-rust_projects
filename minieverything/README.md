# minieverything

[Everything](https://www.voidtools.com/zh-cn/)（voidtools）的简单 CLI 版：NTFS 全盘文件名索引 + 秒级搜索，Rust 实现。

## 工作原理

- **全量建索引**：并行遍历所有固定磁盘 NTFS 卷（约 220 万条 / 20~45s），同时为每个目录采集 64 位 MFT 引用号（file reference number），索引以 bincode 二进制持久化到 `%LOCALAPPDATA%\minieverything\index.bin`。
- **增量更新**：通过 NTFS **USN Journal**（变更日志）从上次游标位置补差，把 CREATE / DELETE / RENAME 记录应用到索引。目录改名时整棵子树路径自动前缀重写。
- **搜索**：内存子串匹配，220 万条约 300~400ms；支持通配符（模式含 `*` `?` 时自动启用）与正则（`-r`）。

> USN 增量需要管理员权限（打开 `\\.\C:` 卷设备）。无权限时自动降级：索引只读查询，`update` 提示以管理员运行。

## 用法

```powershell
# 首次建索引（建议管理员身份，可同时启用 USN 游标）
minieverything update

# 直接搜索（默认搜索前自动做 USN 增量刷新）
minieverything cargo.toml

# 常用选项
minieverything "s*.rs"              # 通配符
minieverything "^ma" -r             # 正则
minieverything src -t dir           # 只搜目录
minieverything mini -l 20           # 限制条数（默认 100，0 = 不限）
minieverything log --full-path      # 匹配完整路径而非文件名
minieverything LOG -c               # 区分大小写
minieverything foo --no-update      # 跳过增量刷新，纯离线查询

# 索引管理
minieverything update --rebuild     # 强制全量重建
minieverything status               # 查看索引状态与各卷 USN 游标
```

## 构建

```powershell
cargo build --release
# 产物：target\release\minieverything.exe
```

## 已知限制（一期范围）

- 仅索引固定磁盘上的 NTFS 卷（USN Journal 依赖；exFAT/FAT32/网络盘不索引）。
- USN Journal 环形缓冲被覆盖、或 journal 被重建时，对应卷自动全量重扫兜底。
- 文件改名若以「单条合并记录」形式出现（罕见），旧路径可能残留至下次全量重建。
- 无 `SELECT *` 问题 😄：查询全部在内存中完成。

## 测试

```powershell
cargo test    # 9 个单元测试：USN 记录应用逻辑（增/删/改/移动/前缀碰撞）
```
