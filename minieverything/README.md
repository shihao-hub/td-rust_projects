# minieverything

[Everything](https://www.voidtools.com/zh-cn/)（voidtools）的简单 CLI 版：NTFS 全盘文件名索引 + 秒级搜索，Rust 实现。

## 工作原理

- **全量建索引**：并行遍历所有固定磁盘 NTFS 卷（约 220 万条 / 20~45s），同时为每个目录采集 64 位 MFT 引用号（file reference number），索引以 bincode 二进制持久化到 `%APPDATA%\language_projects\minieverything\index.bin`（取不到 APPDATA 回退 `~/.language_projects/minieverything/`）。
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

## 构建与打包

### 开发阶段（debug exe）

```powershell
cargo build                 # 增量编译，改完代码秒级出包
# 产物：target\debug\minieverything.exe —— 未优化但功能完整，日常开发验证直接跑它

cargo run -- cargo.toml -l 5    # 编译 + 搜索一步到位
cargo test                      # 9 个单元测试：USN 记录应用逻辑（增/删/改/移动/前缀碰撞）
```

debug 小范围建索引（绕过卷枚举，不写 USN 游标，秒级完成）：

```powershell
# 注意：--root 会覆盖现有全量索引（同一个 index.bin），调试前先备份：
#   Copy-Item "$env:APPDATA\language_projects\minieverything\index.bin" index.bin.bak
#   调试完 Move-Item index.bin.bak "$env:APPDATA\language_projects\minieverything\index.bin" -Force
cargo run -- update --root D:\Users\language_projects\rust_projects
```

`status` 中出现「自定义根：...（无 USN 增量）」即说明当前索引为 `--root` 扫描结果；此后不带 `--root` 的 `update` 会自动转全量重建。改 `usn.rs` 后跑 `cargo test` 看记录应用是否仍全绿。

注意：debug 未优化，搜索耗时没有参考价值，性能对比一律用下面的 release 包。

### 生产阶段（release exe）

```powershell
cargo build --release       # 已开启 LTO（Cargo.toml 里 [profile.release] lto = true）
# 产物：target\release\minieverything.exe —— 单文件、无第三方运行时依赖，拷走即用
# （索引数据不随 exe 走，新机器首次使用需 update 建索引）
```

发布前对 release 包做全盘真实验证：

```powershell
# 1) 管理员全量建索引：20~45s，结束后 status 核对条目数（百万级）
#    与各卷 USN 游标（journal_id / next_usn 非 0 才算增量已启用）
.\target\release\minieverything.exe update
.\target\release\minieverything.exe status

# 2) 搜索抽查：普通终端秒级返回（约 300~400ms），退出码 0
.\target\release\minieverything.exe cargo.toml -l 5
.\target\release\minieverything.exe "s*.rs" -l 5
.\target\release\minieverything.exe "^ma" -r -l 5
.\target\release\minieverything.exe src -t dir -l 5

# 3) 增量验证：新建/改名/删除文件后直接搜索，应能命中/消失
New-Item D:\tmp\zz_probe_file.txt -ItemType File -Force
.\target\release\minieverything.exe zz_probe_file            # 应命中
Rename-Item D:\tmp\zz_probe_file.txt zz_probe_renamed.txt
.\target\release\minieverything.exe zz_probe_renamed         # 应命中，旧名不再出现
Remove-Item D:\tmp\zz_probe_renamed.txt
.\target\release\minieverything.exe zz_probe_renamed         # 应无结果
```

降级验证（可选）：普通权限跑 `update`，应提示以管理员运行且不崩；`--no-update` 纯离线查询不受权限影响。

## 已知限制（一期范围）

- 仅索引固定磁盘上的 NTFS 卷（USN Journal 依赖；exFAT/FAT32/网络盘不索引）。
- USN Journal 环形缓冲被覆盖、或 journal 被重建时，对应卷自动全量重扫兜底。
- 文件改名若以「单条合并记录」形式出现（罕见），旧路径可能残留至下次全量重建。
- 无 `SELECT *` 问题 😄：查询全部在内存中完成。
