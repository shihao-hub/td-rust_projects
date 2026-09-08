//! 输出：表格 / JSON / GrantedAccess 解码。
use crate::process::ProcessInfo;
use crate::resolve::FileHandle;
use serde::Serialize;
use std::collections::HashMap;

#[derive(Serialize)]
pub(crate) struct MatchOut {
    pub pid: u32,
    pub process: String,
    pub handle: usize,
    pub access: String,
    pub path: String,
}

#[derive(Serialize)]
pub(crate) struct JsonOut {
    pub query: Option<String>,
    pub matches: Vec<MatchOut>,
    pub scanned_handles: usize,
    pub file_handles: usize,
    pub resolved: usize,
    pub open_fail: usize,
    pub stuck: usize,
    pub elapsed_ms: u64,
}

pub(crate) fn to_matches(
    handles: &[FileHandle],
    procs: &HashMap<u32, ProcessInfo>,
) -> Vec<MatchOut> {
    handles
        .iter()
        .map(|f| MatchOut {
            pid: f.pid,
            process: procs
                .get(&f.pid)
                .map(|p| p.name.clone())
                .unwrap_or_else(|| format!("<{}>", f.pid)),
            handle: f.handle,
            access: access_str(f.access),
            path: f.name.clone(),
        })
        .collect()
}

pub(crate) fn print_json(out: &JsonOut) {
    match serde_json::to_string_pretty(out) {
        Ok(s) => println!("{s}"),
        Err(e) => eprintln!("错误：JSON 序列化失败：{e}"),
    }
}

/// GrantedAccess → 读/写/删 缩写
pub(crate) fn access_str(access: u32) -> String {
    let mut s = String::new();
    if access & (0x8000_0000 | 0x0001) != 0 {
        s.push('R');
    }
    if access & (0x4000_0000 | 0x0002 | 0x0004) != 0 {
        s.push('W');
    }
    if access & (0x0001_0000 | 0x1000_0000) != 0 {
        s.push('D');
    }
    if s.is_empty() {
        s.push('-');
    }
    s
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() > max {
        s.chars().take(max - 1).collect::<String>() + "…"
    } else {
        s.to_string()
    }
}

pub(crate) fn print_table(handles: &[FileHandle], procs: &HashMap<u32, ProcessInfo>) {
    println!(
        "{:<8} {:<24} {:>10}  {:<3} {}",
        "PID", "PROCESS", "HANDLE", "ACC", "PATH"
    );
    for f in handles {
        let name = procs
            .get(&f.pid)
            .map(|p| p.name.as_str())
            .unwrap_or("<unknown>");
        println!(
            "{:<8} {:<24} {:>10X}  {:<3} {}",
            f.pid,
            truncate(name, 24),
            f.handle,
            access_str(f.access),
            f.name
        );
    }
}
