//! minieverything：Everything 的简单 CLI 版。
//! 首次全量遍历建索引，之后通过 NTFS USN Journal 增量补差，实现秒级文件名搜索。

mod index;
mod scan;
mod search;
mod usn;
mod volume;

use clap::{Parser, Subcommand, ValueEnum};
use index::{Index, PathIndex};
use std::time::Instant;

#[derive(Parser)]
#[command(
    name = "minieverything",
    version,
    about = "Everything 的简单 CLI 版：NTFS 全盘文件名索引与秒级搜索"
)]
struct Cli {
    /// 搜索模式（无子命令时直接搜索，如 minieverything cargo.toml）
    pattern: Option<String>,

    /// 最多显示条数，0 表示不限
    #[arg(short = 'l', long, default_value_t = 100)]
    limit: usize,

    /// 区分大小写（默认不区分，与 Windows 文件系统行为一致）
    #[arg(short = 'c', long)]
    case_sensitive: bool,

    /// 按类型过滤：file / dir
    #[arg(short = 't', long, value_enum)]
    entry_type: Option<EntryKindOpt>,

    /// 使用正则表达式匹配
    #[arg(short = 'r', long)]
    regex: bool,

    /// 匹配完整路径而非文件名
    #[arg(long)]
    full_path: bool,

    /// 搜索前跳过 USN 增量刷新（纯离线查询）
    #[arg(long)]
    no_update: bool,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(ValueEnum, Clone, Copy)]
enum EntryKindOpt {
    File,
    Dir,
}

#[derive(Subcommand)]
enum Command {
    /// 建立或更新索引（默认增量；--rebuild 强制全量重建）
    Update {
        /// 强制全量重建
        #[arg(long)]
        rebuild: bool,
        /// 调试用：只扫描指定目录（绕过卷枚举，且不写 USN 游标）
        #[arg(long)]
        root: Option<String>,
    },
    /// 查看索引状态
    Status,
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Some(Command::Update { rebuild, root }) => cmd_update(rebuild, root),
        Some(Command::Status) => cmd_status(),
        None => cmd_search(cli),
    }
}

/// 全量构建索引：遍历全部 NTFS 卷 + 采集目录 ref + 记录 USN 游标
fn full_build(custom_root: Option<String>) -> anyhow::Result<Index> {
    let t0 = Instant::now();

    let (roots, volumes, custom) = match &custom_root {
        Some(r) => {
            eprintln!("扫描自定义根：{r}");
            (vec![r.clone()], Vec::new(), Some(r.clone()))
        }
        None => {
            let vols = volume::enumerate_ntfs_volumes()?;
            if vols.is_empty() {
                anyhow::bail!("未发现任何 NTFS 固定磁盘卷");
            }
            let roots: Vec<String> = vols.iter().map(|v| v.root.clone()).collect();
            (roots, vols, None)
        }
    };

    let mut all_entries = Vec::new();
    let mut all_dirs = std::collections::HashMap::new();
    let mut volume_states = Vec::new();
    let mut total_skipped = 0usize;

    for root in &roots {
        let t = Instant::now();
        eprintln!("扫描 {root} ...");
        let r = scan::scan_root(root);
        eprintln!(
            "  {} 条（跳过 {sk} 条无法访问），耗时 {:.1}s",
            r.entries.len(),
            t.elapsed().as_secs_f64(),
            sk = r.skipped
        );
        total_skipped += r.skipped;
        all_entries.extend(r.entries);
        all_dirs.extend(r.dirs);
    }

    // 记录各卷 USN 游标（先确保 journal 存在；无权限时记 0，后续增量自动降级）
    for v in &volumes {
        let state = match usn::query_journal_state(&v.root) {
            Ok(s) => s,
            Err(e) => {
                eprintln!(
                    "卷 {root} 无法建立 USN 游标：{e}；提示：以管理员身份运行可启用增量更新",
                    root = v.root
                );
                index::VolumeState { root: v.root.clone(), journal_id: 0, next_usn: 0 }
            }
        };
        eprintln!(
            "卷 {root} USN 游标：journal_id={jid}, next_usn={usn}",
            root = v.root,
            jid = state.journal_id,
            usn = state.next_usn
        );
        volume_states.push(state);
    }

    let idx = Index::from_scan(all_entries, all_dirs, volume_states, custom);
    idx.save()?;
    eprintln!(
        "索引已保存：{path:?}（共 {n} 条，目录 {d} 个，跳过 {total_skipped} 条，总耗时 {:.1}s）",
        t0.elapsed().as_secs_f64(),
        path = Index::index_file()?,
        n = idx.entries.len(),
        d = idx.dirs.len(),
    );
    Ok(idx)
}

fn cmd_update(rebuild: bool, custom_root: Option<String>) -> anyhow::Result<()> {
    if custom_root.is_some() || rebuild {
        full_build(custom_root)?;
        return Ok(());
    }

    // 增量更新：无索引或游标不可用时自动全量
    match Index::load()? {
        None => {
            eprintln!("索引不存在，执行全量扫描 ...");
            full_build(None)?;
        }
        Some(mut idx) => {
            if idx.custom_root.is_some() {
                eprintln!("当前索引为自定义根扫描结果，改为全量重建 ...");
                full_build(None)?;
                return Ok(());
            }
            let t0 = Instant::now();
            let mut path_index = PathIndex::default();
            let roots: Vec<String> = idx.volumes.iter().map(|v| v.root.clone()).collect();
            for root in roots {
                match usn::incremental_update_volume(&mut idx, &root, &mut path_index, true) {
                    usn::IncrementalOutcome::Applied(n) => {
                        eprintln!("{root} 增量应用 {n} 条记录")
                    }
                    usn::IncrementalOutcome::Rescanned(reason) => {
                        eprintln!("{root} 触发全量重扫：{reason}")
                    }
                    usn::IncrementalOutcome::Unavailable(m) => {
                        eprintln!("{root} 无法使用 USN 增量：{m}");
                        eprintln!("提示：以管理员身份运行可启用 USN 增量更新");
                    }
                }
            }
            idx.updated_at = chrono::Local::now().timestamp();
            idx.save()?;
            eprintln!("索引已更新（耗时 {:.1}s）", t0.elapsed().as_secs_f64());
        }
    }
    Ok(())
}

fn cmd_status() -> anyhow::Result<()> {
    let path = Index::index_file()?;
    match Index::load()? {
        None => {
            println!("索引不存在：{path:?}");
            println!("请先运行：minieverything update");
        }
        Some(idx) => {
            let (files, dirs) = idx.stats();
            println!("索引文件  : {path:?}");
            println!(
                "条目总数  : {}（文件 {}，目录 {}）",
                idx.entries.len(),
                files,
                dirs
            );
            let dt = chrono::DateTime::from_timestamp(idx.updated_at, 0)
                .map(|d| d.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M:%S").to_string())
                .unwrap_or_else(|| idx.updated_at.to_string());
            println!("更新时间  : {dt}");
            if let Some(r) = &idx.custom_root {
                println!("自定义根  : {r}（无 USN 增量）");
            }
            println!("卷游标    :");
            for v in &idx.volumes {
                println!(
                    "  {} journal_id={} next_usn={}",
                    v.root, v.journal_id, v.next_usn
                );
            }
        }
    }
    Ok(())
}

fn cmd_search(cli: Cli) -> anyhow::Result<()> {
    let pattern = match &cli.pattern {
        Some(p) => p.clone(),
        None => {
            // 无参数无子命令：打印帮助
            let _ = Cli::parse_from(["minieverything", "--help"]);
            return Ok(());
        }
    };

    // 加载索引
    let mut idx = match Index::load()? {
        Some(i) => i,
        None => {
            eprintln!("索引不存在，请先运行：minieverything update");
            std::process::exit(2);
        }
    };

    // 搜索前增量刷新（可用则应用并回写游标；不触发全量重扫）
    if !cli.no_update && idx.custom_root.is_none() {
        let mut path_index = PathIndex::default();
        let roots: Vec<String> = idx.volumes.iter().map(|v| v.root.clone()).collect();
        let mut refreshed = false;
        for root in roots {
            if let usn::IncrementalOutcome::Applied(n) =
                usn::incremental_update_volume(&mut idx, &root, &mut path_index, false)
            {
                refreshed |= n > 0;
            }
        }
        if refreshed {
            idx.updated_at = chrono::Local::now().timestamp();
            idx.save()?;
        }
    }

    let opts = search::SearchOptions {
        case_sensitive: cli.case_sensitive,
        use_regex: cli.regex,
        entry_kind: cli.entry_type.map(|k| match k {
            EntryKindOpt::File => search::EntryKind::File,
            EntryKindOpt::Dir => search::EntryKind::Dir,
        }),
        full_path: cli.full_path,
        limit: cli.limit,
    };

    let matcher = search::build_matcher(&pattern, &opts)?;
    let t0 = Instant::now();
    let stats = search::run_search(&idx.entries, &matcher, &opts);
    eprintln!(
        "显示 {shown} 条{more}（扫描 {scanned} 条，耗时 {ms:.0} ms）",
        shown = stats.shown,
        more = if stats.truncated { "（已达 --limit 上限）" } else { "" },
        scanned = stats.total_scanned,
        ms = t0.elapsed().as_secs_f64() * 1000.0,
    );
    Ok(())
}
