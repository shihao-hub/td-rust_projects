//! 句柄名称解析：DuplicateHandle 到自身进程后 NtQueryObject 查询对象名。
//! NtQueryObject 对命名管道等对象可能永久阻塞——用"牺牲线程"保护：
//! 协调者把重复句柄放入队列，查询线程循环取出逐个查询；
//! 单个结果超过 timeout 即认为该句柄卡死：遗弃当前查询线程（泄漏一个栈），
//! 另起新线程继续消化剩余队列。绝不使用 TerminateThread（loader lock 风险）。
//! 相比逐句柄起线程，线程创建次数从 O(n) 降到 O(卡死数)。
use crate::winapi::{self, H};
use std::collections::{HashMap, VecDeque};
use std::ffi::c_void;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

/// 注意：不要按 GrantedAccess 过滤"可疑管道掩码"（如 0x0012019F）——
/// 那其实是 FILE_GENERIC_READ|WRITE 的映射值，会漏掉所有读写打开的文件；
/// 挂死风险由牺牲线程超时机制兜底。
#[derive(Clone, Copy)]
pub(crate) struct Candidate {
    pub pid: u32,
    pub handle: usize,
    pub access: u32,
}

pub(crate) struct FileHandle {
    pub pid: u32,
    pub handle: usize,
    pub access: u32,
    /// 对象名（设备路径形式，调用方再做 DOS/UNC 映射）
    pub name: String,
}

#[derive(Default)]
pub(crate) struct ResolveStats {
    pub resolved: usize,
    pub open_fail: usize,
    pub stuck: usize,
    pub unnamed: usize,
}

/// 跨线程队列里的重复句柄
struct DupHandle(H);
unsafe impl Send for DupHandle {}
#[inline]
fn dup_raw(d: DupHandle) -> H {
    d.0
}

pub(crate) fn resolve_all(
    cands: &[Candidate],
    timeout: Duration,
) -> (Vec<FileHandle>, ResolveStats) {
    let mut stats = ResolveStats::default();
    if cands.is_empty() {
        return (Vec::new(), stats);
    }
    // I/O 密集：分片数取 2×核数（协调者很轻），每分片多个并行查询线程掩盖卡死句柄
    let workers = thread::available_parallelism()
        .map(|n| n.get() * 2)
        .unwrap_or(16)
        .clamp(1, 64);
    let chunk = cands.len().div_ceil(workers).max(1);
    let mut out = Vec::new();
    thread::scope(|s| {
        let joins: Vec<_> = cands
            .chunks(chunk)
            .map(|shard| s.spawn(move || resolve_shard(shard, timeout)))
            .collect();
        for j in joins {
            match j.join() {
                Ok((mut v, open_fail, stuck, unnamed)) => {
                    stats.resolved += v.len();
                    stats.open_fail += open_fail;
                    stats.stuck += stuck;
                    stats.unnamed += unnamed;
                    out.append(&mut v);
                }
                Err(_) => eprintln!("警告：解析工作线程异常退出"),
            }
        }
    });
    (out, stats)
}

type QueryResult = (usize, Result<Option<String>, i32>);

fn resolve_shard(cands: &[Candidate], timeout: Duration) -> (Vec<FileHandle>, usize, usize, usize) {
    let mut out = Vec::new();
    let mut open_fail = 0usize;
    let mut unnamed = 0usize;

    // 1) 一次性完成 DuplicateHandle（快，进程句柄缓存；失败计 open_fail）
    let mut proc_cache: HashMap<u32, Option<H>> = HashMap::new();
    let mut queue: VecDeque<(usize, DupHandle)> = VecDeque::with_capacity(cands.len());
    for (idx, c) in cands.iter().enumerate() {
        let ph = *proc_cache.entry(c.pid).or_insert_with(|| {
            let h = unsafe { winapi::OpenProcess(winapi::PROCESS_DUP_HANDLE, 0, c.pid) };
            if h.is_null() { None } else { Some(h) }
        });
        let Some(ph) = ph else {
            open_fail += 1;
            continue;
        };
        let mut dup: H = std::ptr::null_mut();
        let ok = unsafe {
            winapi::DuplicateHandle(
                ph,
                c.handle as H,
                winapi::GetCurrentProcess(),
                &mut dup,
                0,
                0,
                winapi::DUPLICATE_SAME_ACCESS,
            )
        };
        if ok == 0 || dup.is_null() {
            open_fail += 1;
            continue;
        }
        queue.push_back((idx, DupHandle(dup)));
    }
    let queued = queue.len();

    // 2) 查询线程消化队列；协调者按结果收货，超时即换新线程。
    //    每分片预启动 K 个并行查询线程：单个句柄卡死时其余线程继续产出，
    //    协调者无需为每个卡死句柄白等一个超时窗口。
    const QUERY_THREADS_PER_SHARD: usize = 8;
    let queue = Arc::new(Mutex::new(queue));
    let (tx, rx) = mpsc::channel::<QueryResult>();
    for _ in 0..QUERY_THREADS_PER_SHARD {
        spawn_query_thread(queue.clone(), tx.clone());
    }

    let mut received = 0usize;
    let mut idle_rounds = 0usize;
    let mut respawns = 0usize;
    const MAX_RESPAWNS: usize = 64;
    loop {
        let queue_empty = queue.lock().map(|q| q.is_empty()).unwrap_or(true);
        if queue_empty && idle_rounds >= 2 {
            break; // 队列清空且连续两轮无结果：余下均为卡死句柄
        }
        match rx.recv_timeout(timeout) {
            Ok((idx, Ok(Some(name)))) => {
                let c = &cands[idx];
                out.push(FileHandle {
                    pid: c.pid,
                    handle: c.handle,
                    access: c.access,
                    name,
                });
                received += 1;
                idle_rounds = 0;
            }
            Ok((_, Ok(None))) => {
                unnamed += 1;
                received += 1;
                idle_rounds = 0;
            }
            Ok((_, Err(_))) => {
                received += 1; // 查询失败（句柄已失效等）
                idle_rounds = 0;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                // 有句柄卡死/极慢：遗弃当前查询线程，另起线程继续消化（设总量上限防线程膨胀）
                if !queue_empty && respawns < MAX_RESPAWNS {
                    spawn_query_thread(queue.clone(), tx.clone());
                    respawns += 1;
                }
                idle_rounds += 1;
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break, // 所有查询线程已退出
        }
    }
    let stuck = queued.saturating_sub(received);

    // 3) 清理：队列里未消化的重复句柄关闭；进程句柄关闭
    if let Ok(mut q) = queue.lock() {
        while let Some((_, d)) = q.pop_front() {
            unsafe { winapi::CloseHandle(dup_raw(d)) };
        }
    }
    drop(tx);
    for h in proc_cache.values().flatten() {
        unsafe { winapi::CloseHandle(*h) };
    }
    (out, open_fail, stuck, unnamed)
}

fn spawn_query_thread(
    queue: Arc<Mutex<VecDeque<(usize, DupHandle)>>>,
    tx: mpsc::Sender<QueryResult>,
) {
    let builder = thread::Builder::new().stack_size(64 * 1024);
    if builder
        .spawn(move || {
            loop {
                let item = queue.lock().map(|mut q| q.pop_front()).unwrap_or(None);
                let Some((idx, d)) = item else { return };
                let dup = dup_raw(d);
                let r = query_object_name(dup);
                unsafe { winapi::CloseHandle(dup) };
                if tx.send((idx, r)).is_err() {
                    return; // 协调者已离开
                }
            }
        })
        .is_err()
    {
        eprintln!("警告：查询线程创建失败");
    }
}

const OBJECT_NAME_INFORMATION: u32 = 1;

/// 8 字节对齐的名称缓冲（UnicodeString 含指针，必须对齐读取）
#[repr(C, align(8))]
struct AlignedNameBuf([u8; 2048]);

fn query_object_name(h: H) -> Result<Option<String>, i32> {
    let mut buf = AlignedNameBuf([0u8; 2048]);
    let mut ret = 0u32;
    let st = unsafe {
        winapi::NtQueryObject(
            h,
            OBJECT_NAME_INFORMATION,
            buf.0.as_mut_ptr().cast::<c_void>(),
            2048,
            &mut ret,
        )
    };
    match st {
        winapi::STATUS_SUCCESS => parse_object_name(&buf.0),
        winapi::STATUS_BUFFER_OVERFLOW | winapi::STATUS_INFO_LENGTH_MISMATCH => {
            // 名称过长：按 ReturnLength 放大重试
            let words = (ret as usize + 72) / 8;
            let mut big = vec![0u64; words];
            let st2 = unsafe {
                winapi::NtQueryObject(
                    h,
                    OBJECT_NAME_INFORMATION,
                    big.as_mut_ptr().cast::<c_void>(),
                    (words * 8) as u32,
                    &mut ret,
                )
            };
            match st2 {
                winapi::STATUS_SUCCESS | winapi::STATUS_BUFFER_OVERFLOW => {
                    let bytes =
                        unsafe { std::slice::from_raw_parts(big.as_ptr().cast::<u8>(), words * 8) };
                    parse_object_name(bytes)
                }
                winapi::STATUS_OBJECT_NAME_NOT_FOUND => Ok(None),
                other => Err(other),
            }
        }
        winapi::STATUS_OBJECT_NAME_NOT_FOUND => Ok(None), // 无名称对象
        other => Err(other),
    }
}

/// 解析 OBJECT_NAME_INFORMATION：[UNICODE_STRING][名称字符...]
fn parse_object_name(info: &[u8]) -> Result<Option<String>, i32> {
    let us = unsafe { &*(info.as_ptr().cast::<winapi::UnicodeString>()) };
    let chars = us.Length as usize / 2;
    if chars == 0 || us.Buffer.is_null() {
        return Ok(None);
    }
    let s = unsafe { std::slice::from_raw_parts(us.Buffer, chars) };
    Ok(Some(String::from_utf16_lossy(s)))
}
