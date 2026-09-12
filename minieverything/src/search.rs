//! 搜索匹配：子串（默认）/ 通配符（含 * ? 时）/ 正则（--regex）。

use crate::index::{basename, IndexEntry};

/// 条目类型过滤
#[derive(Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum EntryKind {
    File,
    Dir,
}

/// 编译后的匹配器
pub enum Matcher {
    Substring { needle: String },
    Wildcard { pat: glob::Pattern },
    Regex { re: regex::Regex },
}

/// 搜索选项
pub struct SearchOptions {
    pub case_sensitive: bool,
    pub use_regex: bool,
    pub entry_kind: Option<EntryKind>,
    pub full_path: bool,
    pub limit: usize, // 0 = 不限
}

/// 依据原始 pattern 与选项构造匹配器。
/// 非 regex 模式下，含 `*` 或 `?` 时自动切换为通配符匹配（文件名语义）。
pub fn build_matcher(pattern: &str, opts: &SearchOptions) -> anyhow::Result<Matcher> {
    let pat_src = if opts.case_sensitive {
        pattern.to_string()
    } else {
        pattern.to_lowercase()
    };

    if opts.use_regex {
        let re = if opts.case_sensitive {
            regex::Regex::new(pattern)?
        } else {
            regex::RegexBuilder::new(pattern)
                .case_insensitive(true)
                .build()?
        };
        return Ok(Matcher::Regex { re });
    }

    if pattern.contains('*') || pattern.contains('?') {
        let pat = glob::Pattern::new(&pat_src)
            .map_err(|e| anyhow::anyhow!("通配符模式无效：{e}"))?;
        return Ok(Matcher::Wildcard { pat });
    }

    Ok(Matcher::Substring { needle: pat_src })
}

impl Matcher {
    /// target 的形态由调用方决定（basename 或 full path）；
    /// 大小写不敏感时由调用方预先 lowercase。
    fn matches(&self, target_lower: &str) -> bool {
        match self {
            Matcher::Substring { needle } => target_lower.contains(needle.as_str()),
            Matcher::Wildcard { pat } => pat.matches(target_lower),
            Matcher::Regex { re } => re.is_match(target_lower),
        }
    }
}

/// 搜索结果
pub struct SearchStats {
    pub shown: usize,
    pub total_scanned: usize,
    pub truncated: bool,
}

/// 在索引上执行搜索并打印结果
pub fn run_search(index: &[IndexEntry], matcher: &Matcher, opts: &SearchOptions) -> SearchStats {
    let mut stats = SearchStats { shown: 0, total_scanned: 0, truncated: false };

    for e in index {
        if let Some(kind) = opts.entry_kind {
            let ok = match kind {
                EntryKind::File => !e.is_dir,
                EntryKind::Dir => e.is_dir,
            };
            if !ok {
                continue;
            }
        }
        let target = if opts.full_path { e.path.as_str() } else { basename(&e.path) };
        let target = if opts.case_sensitive {
            target.to_string()
        } else {
            target.to_lowercase()
        };
        stats.total_scanned += 1;
        if matcher.matches(&target) {
            println!("{}", e.path);
            stats.shown += 1;
            if opts.limit > 0 && stats.shown >= opts.limit {
                stats.truncated = true;
                break;
            }
        }
    }
    stats
}
