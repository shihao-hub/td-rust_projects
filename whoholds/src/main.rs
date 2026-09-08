mod dospath;
mod output;
mod probe;
mod process;
mod resolve;
mod snapshot;
mod winapi;

use clap::Parser;
use std::collections::BTreeMap;
use std::time::{Duration, Instant};

/// whoholds —— Windows 文件句柄占用检测（类似 Sysinternals handle.exe）
#[derive(Parser)]
#[command(name = "whoholds", version, about = "检测哪个进程占用了指定文件")]
struct Args {
    /// 目标文件或目录路径（目录则匹配其下所有占用）
    path: Option<String>,
    /// 列出指定进程打开的所有文件
    #[arg(long)]
    pid: Option<u32>,
    /// 枚举全系统所有文件句柄
    #[arg(long)]
    all: bool,
    /// 以 JSON 输出结果
    #[arg(long)]
    json: bool,
    /// 单个句柄名称查询的超时毫秒数（本地路径通常 <5ms；网络路径可能更慢，被超时跳过可调大）
    #[arg(long, default_value_t = 25)]
    timeout: u64,
    /// 输出性能与统计信息到 stderr
    #[arg(long)]
    bench: bool,
}

enum Target {
    Path(String),
    Pid(u32),
    All,
}

fn main() {
    // 输出用 UTF-8 代码页，退出前恢复原值
    let orig_cp = unsafe { winapi::GetConsoleOutputCP() };
    unsafe { winapi::SetConsoleOutputCP(winapi::CP_UTF8) };
    let code = run();
    unsafe { winapi::SetConsoleOutputCP(orig_cp) };
    std::process::exit(code);
}

fn run() -> i32 {
    let args = Args::parse();
    let targets = args.path.is_some() as u8 + args.pid.is_some() as u8 + args.all as u8;
    if targets != 1 && !args.bench {
        eprintln!("错误：需要且仅需要一个查询目标：<PATH>、--pid <PID> 或 --all");
        return 2;
    }
    let target = if let Some(p) = &args.path {
        Target::Path(p.clone())
    } else if let Some(pid) = args.pid {
        Target::Pid(pid)
    } else if args.all {
        Target::All
    } else {
        // 仅 --bench：只输出统计
        return bench_only();
    };

    if let Some(step) = winapi::enable_debug_privilege() {
        eprintln!(
            "提示：SeDebugPrivilege 启用失败[{step}]（建议管理员运行），部分进程的句柄将无法解析"
        );
    }

    let t0 = Instant::now();
    // 校准句柄必须在快照之前打开
    let pf = match probe::ProbeFile::open() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("错误：打开校准文件失败：{e}");
            return 2;
        }
    };
    let t_snapshot = Instant::now();
    let snap = match snapshot::HandleSnapshot::take() {
        Ok(s) => s,
        Err(e) => {
            pf.close();
            eprintln!("错误：{e}");
            return 2;
        }
    };
    let snapshot_ms = t_snapshot.elapsed().as_millis();

    let fti = match pf.type_index_in(&snap) {
        Ok(i) => i,
        Err(e) => {
            pf.close();
            eprintln!("错误：{e}");
            return 2;
        }
    };
    let probe_value = pf.value;
    pf.close();

    let t_proc = Instant::now();
    // 进程表与句柄解析并行：仅在输出时需要进程名
    let procs_handle = std::thread::spawn(process::process_map);

    // 候选过滤：File 类型 + 排除校准句柄自身
    // （不按 access 过滤：0x0012019F 等掩码是正常读写文件，挂死句柄靠超时机制兜底）
    let my_pid = unsafe { winapi::GetCurrentProcessId() } as usize;
    let mut candidates: Vec<resolve::Candidate> = snap
        .entries()
        .iter()
        .filter(|e| e.type_index == fti)
        .filter(|e| !(e.pid == my_pid && e.handle == probe_value))
        .map(|e| resolve::Candidate {
            pid: e.pid as u32,
            handle: e.handle,
            access: e.access,
        })
        .collect();
    let file_candidates = candidates.len();
    if let Target::Pid(pid) = target {
        candidates.retain(|c| c.pid == pid);
    }

    let t_resolve = Instant::now();
    let (mut resolved, rstats) =
        resolve::resolve_all(&candidates, Duration::from_millis(args.timeout));
    let resolve_ms = t_resolve.elapsed().as_millis();

    let procs = match procs_handle.join() {
        Ok(Ok(p)) => p,
        Ok(Err(e)) => {
            eprintln!("错误：获取进程表失败：{e}");
            return 2;
        }
        Err(_) => {
            eprintln!("错误：进程表线程异常退出");
            return 2;
        }
    };
    let proc_ms = t_proc.elapsed().as_millis();

    let mapper = dospath::DosPathMapper::system();
    for f in &mut resolved {
        if let Some(m) = mapper.map(&f.name) {
            f.name = m;
        }
    }
    resolved.sort_by(|a, b| (a.pid, &a.name).cmp(&(b.pid, &b.name)));
    let total_ms = t0.elapsed().as_millis();

    // 按目标筛选结果；对精确文件查询附带独占探测，用于无命中时的定性提示
    let mut file_probe: Option<ProbeOutcome> = None;
    let (hits, query_label): (Vec<resolve::FileHandle>, Option<String>) = match target {
        Target::Path(q) => {
            let nq = normalize_query(&q);
            // 尾部 * → glob 前缀（不带分隔符）；尾部 \ 或已存在目录 → 目录前缀（带分隔符）
            let ends_star = nq.ends_with('*');
            let is_prefix = ends_star
                || nq.ends_with('\\')
                || std::fs::metadata(&nq).map(|m| m.is_dir()).unwrap_or(false);
            if !is_prefix && std::fs::metadata(&nq).is_ok() {
                file_probe = Some(probe_exclusive(&nq));
            }
            let needle = nq.trim_end_matches('\\').trim_end_matches('*').to_string();
            let pat = if is_prefix && !needle.ends_with('\\') {
                format!("{needle}\\")
            } else {
                needle
            };
            let pat = if ends_star {
                pat.trim_end_matches('\\').to_string()
            } else {
                pat
            };
            let hits = resolved
                .into_iter()
                .filter(|f| {
                    let lname = f.name.to_lowercase();
                    if is_prefix {
                        lname.starts_with(&pat)
                    } else {
                        lname == pat
                    }
                })
                .collect();
            (hits, Some(q))
        }
        Target::Pid(_) => (resolved, None),
        Target::All => (resolved, None),
    };

    if args.json {
        let out = output::JsonOut {
            query: query_label,
            matches: output::to_matches(&hits, &procs),
            scanned_handles: snap.total(),
            file_handles: file_candidates,
            resolved: rstats.resolved,
            open_fail: rstats.open_fail,
            stuck: rstats.stuck,
            elapsed_ms: total_ms as u64,
        };
        output::print_json(&out);
    } else {
        output::print_table(&hits, &procs);
    }

    eprintln!(
        "句柄 {} 条（文件 {}），解析成功 {}，进程不可访问 {}，超时跳过 {}，耗时 {}ms",
        snap.total(),
        file_candidates,
        rstats.resolved,
        rstats.open_fail,
        rstats.stuck,
        total_ms
    );
    if args.bench {
        eprintln!(
            "[bench] 快照 {snapshot_ms}ms / 进程表 {proc_ms}ms / 解析 {resolve_ms}ms / 无名 {} / 候选 {}",
            rstats.unnamed,
            candidates.len()
        );
    }
    if hits.is_empty() {
        match file_probe {
            Some(ProbeOutcome::Free) => {
                eprintln!("提示：文件当前可独占打开，没有进程占用它");
            }
            Some(ProbeOutcome::SharingViolation) => {
                eprintln!(
                    "提示：文件存在共享冲突，但占用者位于当前无权限访问的进程中；请以管理员身份重试"
                );
            }
            _ => {}
        }
    }
    if hits.is_empty() { 1 } else { 0 }
}

/// 精确文件查询无命中时的定性探测
enum ProbeOutcome {
    /// GENERIC_READ + 共享全无独占打开成功：确凿无人占用（读写语义）
    Free,
    /// ERROR_SHARING_VIOLATION：确凿被占用但扫描未捕获（典型：占用者在无权限进程）
    SharingViolation,
}

fn probe_exclusive(path: &str) -> ProbeOutcome {
    let wide = winapi::to_wide(path);
    let h = unsafe {
        winapi::CreateFileW(
            wide.as_ptr(),
            winapi::GENERIC_READ,
            0, // FILE_SHARE_NONE
            std::ptr::null(),
            winapi::OPEN_EXISTING,
            winapi::FILE_ATTRIBUTE_NORMAL,
            std::ptr::null_mut(),
        )
    };
    if h.is_null() || h == winapi::INVALID_HANDLE_VALUE {
        if unsafe { winapi::GetLastError() } == 32 {
            ProbeOutcome::SharingViolation
        } else {
            ProbeOutcome::Free // 拒绝访问等其他错误：不下结论
        }
    } else {
        unsafe { winapi::CloseHandle(h) };
        ProbeOutcome::Free
    }
}

/// 查询路径归一化：统一分隔符、去引号/\\?\ 前缀、相对转绝对、折叠重复分隔符、
/// 展开 8.3 短名、小写
fn normalize_query(p: &str) -> String {
    let mut s = p.trim().trim_matches('"').replace('/', "\\");
    if let Some(stripped) = s.strip_prefix("\\\\?\\") {
        s = stripped.to_string();
    }
    if !std::path::Path::new(&s).is_absolute() {
        if let Ok(cwd) = std::env::current_dir() {
            s = cwd.join(&s).to_string_lossy().into_owned();
        }
    }
    // 规范化：折叠 "C:\\Users"（用户从 JSON/C# 字符串拷来的双杠）等重复分隔符
    if let Some(canon) = full_path_name(&s) {
        s = canon;
    }
    // 内核对象名总是长名形式：$env:TEMP 等常见 8.3 短名必须展开，否则精确匹配必挂。
    // 含通配符的路径 GetLongPathNameW 会失败：只展开存在的父目录，通配尾巴保留
    let (base, tail) = match s.rfind('\\') {
        Some(pos) if s[pos..].contains('*') || s[pos..].contains('?') => {
            (s[..pos].to_string(), s[pos..].to_string())
        }
        _ => (s.clone(), String::new()),
    };
    if !base.is_empty() {
        if let Some(long) = long_path(&base) {
            s = long + &tail;
        }
    }
    s.to_lowercase()
}

/// GetFullPathNameW 规范化路径（折叠重复分隔符等）；失败返回 None
fn full_path_name(s: &str) -> Option<String> {
    let wide = winapi::to_wide(s);
    let mut out = vec![0u16; 1024];
    let n = unsafe {
        winapi::GetFullPathNameW(
            wide.as_ptr(),
            out.len() as u32,
            out.as_mut_ptr(),
            std::ptr::null_mut(),
        )
    };
    if n > out.len() as u32 {
        out = vec![0u16; n as usize + 1];
        let n2 = unsafe {
            winapi::GetFullPathNameW(
                wide.as_ptr(),
                out.len() as u32,
                out.as_mut_ptr(),
                std::ptr::null_mut(),
            )
        };
        if n2 == 0 || n2 > out.len() as u32 {
            return None;
        }
        return Some(String::from_utf16_lossy(&out[..n2 as usize]));
    }
    if n > 0 {
        Some(String::from_utf16_lossy(&out[..n as usize]))
    } else {
        None
    }
}

/// GetLongPathNameW 展开 8.3 短名；文件不存在等失败返回 None（保持原样）
fn long_path(s: &str) -> Option<String> {
    let wide = winapi::to_wide(s);
    let mut out = vec![0u16; 1024];
    let n = unsafe { winapi::GetLongPathNameW(wide.as_ptr(), out.as_mut_ptr(), out.len() as u32) };
    if n > out.len() as u32 {
        out = vec![0u16; n as usize + 1];
        let n2 =
            unsafe { winapi::GetLongPathNameW(wide.as_ptr(), out.as_mut_ptr(), out.len() as u32) };
        if n2 > 0 && n2 <= out.len() as u32 {
            return Some(String::from_utf16_lossy(&out[..n2 as usize]));
        }
        return None;
    }
    if n > 0 {
        Some(String::from_utf16_lossy(&out[..n as usize]))
    } else {
        None
    }
}

fn bench_only() -> i32 {
    if let Some(step) = winapi::enable_debug_privilege() {
        eprintln!(
            "提示：SeDebugPrivilege 启用失败[{step}]（建议管理员运行），部分进程的句柄将无法解析"
        );
    }
    let pf = match probe::ProbeFile::open() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("错误：打开校准文件失败：{e}");
            return 2;
        }
    };
    let t_snapshot = Instant::now();
    let snap = match snapshot::HandleSnapshot::take() {
        Ok(s) => s,
        Err(e) => {
            pf.close();
            eprintln!("错误：{e}");
            return 2;
        }
    };
    let snapshot_ms = t_snapshot.elapsed().as_millis();
    let fti = match pf.type_index_in(&snap) {
        Ok(i) => i,
        Err(e) => {
            pf.close();
            eprintln!("错误：{e}");
            return 2;
        }
    };
    pf.close();
    let t_proc = Instant::now();
    let procs = match process::process_map() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("错误：获取进程表失败：{e}");
            return 2;
        }
    };
    let proc_ms = t_proc.elapsed().as_millis();

    let mut per_type: BTreeMap<u16, usize> = BTreeMap::new();
    let mut file_count = 0usize;
    for e in snap.entries() {
        *per_type.entry(e.type_index).or_default() += 1;
        if e.type_index == fti {
            file_count += 1;
        }
    }
    let mut tops: Vec<(u16, usize)> = per_type.into_iter().collect();
    tops.sort_by(|a, b| b.1.cmp(&a.1));
    let tops: Vec<String> = tops
        .iter()
        .take(10)
        .map(|(t, c)| format!("{}:{}", t, c))
        .collect();
    println!("句柄总数: {}", snap.total());
    println!("文件句柄: {file_count} (type_index={fti})");
    println!("进程数: {}", procs.len());
    println!("type_index 分布(前10): {}", tops.join("  "));
    println!("耗时: 快照 {snapshot_ms}ms + 进程表 {proc_ms}ms");
    0
}
