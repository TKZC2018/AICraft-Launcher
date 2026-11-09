//! 资源补全 —— 依据校验报告下载缺失文件。
//!
//! 只用官方源：
//! - assets:  `https://resources.download.minecraft.net/<xx>/<hash>`
//! - 库:      `https://libraries.minecraft.net/`
//!
//! 镜像（BMCLAPI 等）留作后续，因为它需要对 URL 做重写，且不同镜像的
//! 路径规则不一致，先不引入这层复杂度。
//!
//! 并发策略：直接开固定数量线程。下载是 IO 密集，但要避免把对端打满，
//! 默认 6 个并发（Mojang CDN 对单 IP 有连接数限制）。

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use crate::settings::Mirror;
use crate::speed::Budget;
use crate::transport::{self, FetchError};
use crate::verify::{MissingItem, Problem};

/// 补全配置。
#[derive(Debug, Clone)]
pub struct RepairConfig {
    /// 并发线程数。
    pub concurrency: usize,
    /// 是否只打印计划、不实际下载。
    pub dry_run: bool,
    /// 版本下载源；补全时按它改写地址。
    pub mirror: Mirror,
    /// 下载总限速（KB/s）；`None` 表示不限。
    pub limit_kbps: Option<u32>,
}

impl Default for RepairConfig {
    fn default() -> Self {
        RepairConfig {
            concurrency: 6,
            dry_run: false,
            mirror: Mirror::Official,
            limit_kbps: None,
        }
    }
}

/// 单个条目的补全结果。
#[derive(Debug, Clone)]
pub struct RepairResult {
    pub item: MissingItem,
    pub outcome: RepairOutcome,
}

/// 补全结果类型。
#[derive(Debug, Clone)]
pub enum RepairOutcome {
    /// 下载成功。
    Downloaded { bytes: u64 },
    /// 已存在，跳过。
    Skipped,
    /// 无法补全（没有下载地址）。
    NoUrl,
    /// 失败。
    Failed { message: String },
}

/// 补全汇总。
#[derive(Debug, Default)]
pub struct RepairReport {
    pub results: Vec<RepairResult>,
    pub downloaded: usize,
    pub skipped: usize,
    pub failed: usize,
    pub no_url: usize,
    pub bytes: u64,
}

impl RepairReport {
    pub fn is_success(&self) -> bool {
        self.failed == 0 && self.no_url == 0
    }
}

/// 下载进度事件。
///
/// 回调由 `repair_with_progress` 在下载过程中推送，供 UI 实时更新。
/// 刻意用值类型而非引用，因为回调要跨越线程边界。
#[derive(Debug, Clone)]
pub enum Progress {
    /// 开始处理某一条目（第 index 个，共 total 个）。
    Start {
        index: usize,
        total: usize,
        path: PathBuf,
    },
    /// 某一条目完成。
    ItemDone {
        index: usize,
        total: usize,
        path: PathBuf,
        outcome: RepairOutcome,
    },
    /// 累计已传输字节数。
    Bytes { total: u64 },
}

impl Progress {
    /// 供 UI 显示的简短描述。
    pub fn describe(&self) -> String {
        match self {
            Progress::Start {
                index,
                total,
                path,
            } => format!(
                "[{}/{}] {}",
                index + 1,
                total,
                path.file_name().and_then(|s| s.to_str()).unwrap_or("?")
            ),
            Progress::ItemDone { outcome, .. } => match outcome {
                RepairOutcome::Downloaded { bytes } => format!("完成 ({} KB)", bytes / 1024),
                RepairOutcome::Skipped => "跳过".to_string(),
                RepairOutcome::NoUrl => "无下载地址".to_string(),
                RepairOutcome::Failed { message } => format!("失败: {}", message),
            },
            Progress::Bytes { total } => format!("已传输 {} MB", total / 1_048_576),
        }
    }
}

/// 依据校验报告补全缺失文件。
///
/// 返回每个条目的处理结果，调用方决定如何展示。
pub fn repair(
    problems: &[MissingItem],
    config: &RepairConfig,
) -> RepairReport {
    repair_with_progress(problems, config, |_| {})
}

/// 带进度回调的补全。
///
/// 回调会在**工作线程**里被调用，调用方不得在里面直接操作 UI 状态；
/// GUI 场景应把事件转发到 channel，由主线程消费。
/// 回调是 `Fn` 而非 `FnMut`，因为多个工作线程会并发调用它。
pub fn repair_with_progress<F>(
    problems: &[MissingItem],
    config: &RepairConfig,
    on_progress: F,
) -> RepairReport
where
    F: Fn(Progress) + Send + Sync + 'static,
{
    let on_progress = Arc::new(on_progress);
    let total = problems.len();

    if config.dry_run || problems.is_empty() {
        let mut report = RepairReport::default();
        for item in problems {
            let outcome = if item.url.is_none() {
                RepairOutcome::NoUrl
            } else {
                RepairOutcome::Skipped
            };
            report.results.push(RepairResult {
                item: item.clone(),
                outcome,
            });
        }
        report.no_url = problems.iter().filter(|p| p.url.is_none()).count();
        return report;
    }

    let queue = Arc::new(Mutex::new(problems.to_vec()));
    let mirror = Arc::new(config.mirror);
    let budget = Budget::new(config.limit_kbps);
    let results = Arc::new(Mutex::new(Vec::new()));
    let bytes_total = Arc::new(AtomicUsize::new(0));
    let done_count = Arc::new(AtomicUsize::new(0));

    let workers = config.concurrency.max(1).min(problems.len());
    let mut handles = Vec::with_capacity(workers);

    for _ in 0..workers {
        let queue = Arc::clone(&queue);
        let mirror = Arc::clone(&mirror);
        let budget = budget.clone();
        let results = Arc::clone(&results);
        let bytes_total = Arc::clone(&bytes_total);
        let done_count = Arc::clone(&done_count);
        let on_progress = Arc::clone(&on_progress);

        handles.push(std::thread::spawn(move || {
            loop {
                // 取一件活干
                let item = {
                    let mut q = queue.lock().unwrap();
                    if q.is_empty() {
                        break;
                    }
                    q.remove(0)
                };

                // 已完成数即本条目的显示序号
                let index = done_count.fetch_add(1, Ordering::Relaxed);

                on_progress(Progress::Start {
                    index,
                    total,
                    path: item.path.clone(),
                });

                let outcome = match &item.url {
                    None => RepairOutcome::NoUrl,
                    Some(url) => {
                        // 占一个限速名额，随本次下载的作用域归还
                        let share = budget.enter();
                        let fetched = transport::download_with_limit(
                            &mirror.rewrite(url),
                            &item.path,
                            item.sha1.as_deref(),
                            share.kbps(),
                        );
                        drop(share);
                        match fetched {
                            Ok(fetched) => {
                                let acc = bytes_total
                                    .fetch_add(fetched.bytes as usize, Ordering::Relaxed)
                                    as u64
                                    + fetched.bytes;
                                on_progress(Progress::Bytes { total: acc });
                                if fetched.skipped {
                                    RepairOutcome::Skipped
                                } else {
                                    RepairOutcome::Downloaded {
                                        bytes: fetched.bytes,
                                    }
                                }
                            }
                            Err(e) => RepairOutcome::Failed {
                                message: e.to_string(),
                            },
                        }
                    }
                };

                on_progress(Progress::ItemDone {
                    index,
                    total,
                    path: item.path.clone(),
                    outcome: outcome.clone(),
                });

                results.lock().unwrap().push(RepairResult { item, outcome });
            }
        }));
    }

    for h in handles {
        let _ = h.join();
    }

    let mut report = RepairReport {
        results: Arc::try_unwrap(results)
            .map(|m| m.into_inner().unwrap())
            .unwrap_or_default(),
        bytes: bytes_total.load(Ordering::Relaxed) as u64,
        ..Default::default()
    };

    for r in &report.results {
        match &r.outcome {
            RepairOutcome::Downloaded { .. } => report.downloaded += 1,
            RepairOutcome::Skipped => report.skipped += 1,
            RepairOutcome::Failed { .. } => report.failed += 1,
            RepairOutcome::NoUrl => report.no_url += 1,
        }
    }

    // 结果排序：按路径，保证输出稳定
    report.results.sort_by(|a, b| a.item.path.cmp(&b.item.path));

    report
}

/// 格式化一个问题的简短描述。
pub fn describe(item: &MissingItem) -> String {
    let name = item
        .path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("?");
    format!("[{}] {} ({})", item.kind, name, item.reason.as_str())
}

/// 若目标文件已存在但损坏，删掉再下。
///
/// 不复用半截文件：既然校验已经判定它坏了，续传只会拼出更坏的结果。
pub fn prepare_for_download(item: &MissingItem) -> std::io::Result<()> {
    if matches!(item.reason, Problem::WrongSize | Problem::HashMismatch)
        && item.path.is_file()
    {
        std::fs::remove_file(&item.path)?;
    }
    if let Some(parent) = item.path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    Ok(())
}

/// 把 FetchError 转成可读建议。
pub fn suggest_action(err: &FetchError) -> &'static str {
    match err.exit_code {
        Some(6) => "DNS 解析失败，检查网络或代理设置",
        Some(7) => "无法连接服务器，检查防火墙或网络",
        Some(28) => "连接超时，可能网络较慢，稍后重试",
        Some(35) => "TLS 握手失败，可能是系统根证书或代理拦截",
        Some(56) => "传输中断，重试通常可以解决",
        Some(22) => "服务器返回错误状态（可能是文件已不存在）",
        _ => "未知错误，检查网络后重试",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::verify::Problem;
    use std::path::PathBuf;

    fn item(url: Option<&str>) -> MissingItem {
        MissingItem {
            kind: "assets".into(),
            path: PathBuf::from("x/y/z"),
            url: url.map(str::to_string),
            sha1: Some("abc".into()),
            size: Some(10),
            reason: Problem::Missing,
        }
    }

    #[test]
    fn dry_run_does_not_download() {
        let problems = vec![item(Some("https://example/x"))];
        let cfg = RepairConfig {
            concurrency: 2,
            dry_run: true,
            mirror: Mirror::Official,
            limit_kbps: None,
        };
        let report = repair(&problems, &cfg);
        assert_eq!(report.downloaded, 0);
        assert_eq!(report.results.len(), 1);
        assert!(matches!(report.results[0].outcome, RepairOutcome::Skipped));
    }

    #[test]
    fn items_without_url_are_marked_no_url() {
        let problems = vec![item(None)];
        let report = repair(&problems, &RepairConfig::default());
        assert_eq!(report.no_url, 1);
        assert!(!report.is_success());
    }

    #[test]
    fn describe_includes_kind_and_reason() {
        let d = describe(&item(Some("https://a")));
        assert!(d.contains("assets"));
        assert!(d.contains("缺失"));
    }

    #[test]
    fn prepare_removes_corrupt_file() {
        let tmp = crate::testutil::fresh_dir("aicraft_repair_prep");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let f = tmp.join("bad.jar");
        std::fs::write(&f, b"corrupt").unwrap();

        let it = MissingItem {
            kind: "libraries".into(),
            path: f.clone(),
            url: Some("https://x".into()),
            sha1: None,
            size: None,
            reason: Problem::HashMismatch,
        };
        // 删除后需要能重新创建父目录
        prepare_for_download(&it).unwrap();
        assert!(!f.exists());

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn suggest_action_covers_common_codes() {
        for code in [6, 7, 28, 35, 56, 22, 1] {
            let e = FetchError {
                message: "x".into(),
                exit_code: Some(code),
            };
            assert!(!suggest_action(&e).is_empty());
        }
    }

    /// 进度回调应当覆盖每一条目：先 Start 后 ItemDone，且 index 不重复。
    #[test]
    fn progress_reports_every_item() {
        use std::sync::Mutex as StdMutex;

        let problems = vec![item(None), item(None), item(None)];
        let events = Arc::new(StdMutex::new(Vec::new()));
        let events2 = Arc::clone(&events);

        let report = repair_with_progress(&problems, &RepairConfig::default(), move |p| {
            events2.lock().unwrap().push(p);
        });

        assert_eq!(report.no_url, 3);

        let ev = events.lock().unwrap();
        let starts: Vec<_> = ev
            .iter()
            .filter_map(|p| match p {
                Progress::Start { index, .. } => Some(*index),
                _ => None,
            })
            .collect();
        let dones: Vec<_> = ev
            .iter()
            .filter_map(|p| match p {
                Progress::ItemDone { index, .. } => Some(*index),
                _ => None,
            })
            .collect();

        assert_eq!(starts.len(), 3, "每个条目都应有 Start");
        assert_eq!(dones.len(), 3, "每个条目都应有 ItemDone");

        // index 必须互不相同（并发下顺序不确定，但取值应是 0..3）
        let mut sorted = starts.clone();
        sorted.sort_unstable();
        assert_eq!(sorted, vec![0, 1, 2]);
    }

    #[test]
    fn progress_describe_is_non_empty() {
        let p = Progress::Start {
            index: 0,
            total: 5,
            path: PathBuf::from("a/b/c.jar"),
        };
        assert!(p.describe().contains("1/5"));
        assert!(p.describe().contains("c.jar"));

        let d = Progress::ItemDone {
            index: 0,
            total: 1,
            path: PathBuf::from("x"),
            outcome: RepairOutcome::Downloaded { bytes: 2048 },
        };
        assert!(d.describe().contains("KB"));
    }
}
