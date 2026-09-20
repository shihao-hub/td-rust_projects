# whoholds

Windows 文件句柄占用检测 CLI（类似 Sysinternals handle.exe）：查「是哪个进程占用了这个文件」，Rust 实现，零第三方 Win32 绑定（手写 FFI）。

## 工作原理

- `NtQuerySystemInformation(SystemHandleInformation)` 一次性快照全系统句柄（约 54 万条 / ~50ms）；
- 先打开一个校准文件，反推出本机 File 类型的 `type_index`，据此过滤出文件句柄候选；
- 线程池并行 `NtQueryObject` 解析句柄名称，单个句柄带超时（`--timeout`，默认 25ms）兜底挂死句柄；进程表获取与之并行；
- `QueryDosDevice` 把 `\Device\HarddiskVolumeX` 设备路径映射为盘符（`LanmanRedirector` / `Mup` 映射为 UNC 网络路径）；
- 精确文件查询无命中时做定性探测：以「共享全无」独占打开——成功则确凿无人占用；`ERROR_SHARING_VIOLATION` 则占用者在无权限进程中，提示以管理员重试。

> 解析全部进程句柄需要 SeDebugPrivilege（管理员）。普通权限可运行，但会有提示且部分进程句柄无法解析。

## 用法

```powershell
whoholds D:\path\to\file          # 谁占用了这个文件
whoholds D:\path\to\dir\          # 目录前缀匹配（尾部 \ 或目录）
whoholds "D:\path\*.log"          # 尾部 * 通配前缀
whoholds --pid 1234               # 该进程打开的所有文件
whoholds --all                    # 全系统所有文件句柄

whoholds file --json              # JSON 输出（可管道给 jq / ConvertFrom-Json）
whoholds file --timeout 100       # 网络路径解析慢时调大单句柄超时
whoholds --bench                  # 纯统计模式：句柄总数/类型分布/耗时
```

退出码：`0` 有命中，`1` 无命中，`2` 参数或系统错误。stderr 固定输出一行统计（句柄数 / 解析成功 / 超时跳过 / 耗时）。

## 构建与打包

### 开发阶段（debug exe）

```powershell
cargo build                 # 增量编译，改完代码秒级出包
# 产物：target\debug\whoholds.exe —— 未优化但功能完整，日常开发验证直接跑它

cargo run -- --pid $PID     # 编译 + 运行一步到位
cargo run -- --bench        # 快照/解析链路是否正常（普通权限会有 SeDebugPrivilege 提示，属预期）
cargo test                  # 单元测试：DOS 设备路径 → 盘符/UNC 映射逻辑
```

注意：debug 未优化，句柄解析耗时没有参考价值，性能对比一律用下面的 release 包。

### 生产阶段（release exe）

```powershell
cargo build --release       # 已开启 LTO（Cargo.toml 里 [profile.release] lto = true）
# 产物：target\release\whoholds.exe —— 单文件、无第三方运行时依赖，拷走即用
```

发布前对 release 包做真实验证：

```powershell
# 1) 查进程：应列出当前 shell 打开的文件，退出码 0
.\target\release\whoholds.exe --pid $PID

# 2) 查占用：找一个正被占用的文件（如打开中的日志），应命中占用者；
#    查一个空闲文件应提示「可独占打开，没有进程占用它」且退出码 1
.\target\release\whoholds.exe <被占用文件路径>

# 3) 全量 JSON + 性能基线：快照约 50ms 量级，解析耗时随句柄数浮动
.\target\release\whoholds.exe --all --json
.\target\release\whoholds.exe --bench
```

管理员身份下运行不应再出现 SeDebugPrivilege 提示，且「进程不可访问」计数应显著下降。

### 免 UAC 提权（scripts/）

```powershell
# 管理员运行一次：注册最高权限计划任务（仅一次 UAC）
scripts\install-task.cmd

# 之后普通终端即可免 UAC 提权查询，输出与退出码透传
scripts\whoholds-admin.cmd <文件路径>
scripts\whoholds-admin.cmd <文件路径> --json
```

前提是 `target\release\whoholds.exe` 已构建；脚本用 `%TEMP%` 中转请求与输出，等待上限约 180s。

## 已知限制

- 挂死句柄靠超时机制兜底，被跳过的句柄不会出现在结果中（可从 stderr 统计的「超时跳过」看出）；
- 普通权限下，占用者位于高权限进程时无法定位，仅能给出「存在共享冲突」的定性提示；
- 仅适用 Windows（依赖 NT 原生 API 与 DOS 设备路径语义）。
