//! 检查并更新已经装好的模组。
//!
//! 认模组的办法是**按文件哈希反查**：Modrinth 的 `/v2/version_files` 收一组
//! SHA-1，回来的是每个文件属于哪个项目、哪一版。这样不需要模组自己声明什么，
//! 也不需要玩家手动关联——jar 在盘上，就能认出来。
//!
//! 反查不到的（自己编译的、改过的、别处下的）一律标成「不认得」，不猜、
//! 也不动它们。

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::json::{self, Json};
use crate::loader::MODRINTH_API;
use crate::modrinth::{self, ProjectKind};

/// 一次反查 / 补名字最多塞多少个。Modrinth 的上限是 100。
const BATCH: usize = 100;

/// 同时问几个项目的版本列表。
///
/// 每个项目要单独发一次请求（Modrinth 没有「批量问最新版」的接口），
/// 52 个模组串行要十几秒；8 路并发把这段压到两秒左右，再高容易撞限流。
const WORKERS: usize = 8;

/// 一个已装模组的检查结果。
#[derive(Debug, Clone)]
pub struct Check {
    /// 盘上那个 jar
    pub path: PathBuf,
    pub filename: String,
    /// 项目名（界面显示「Sodium」而不是一串 id）；认不出时为 `None`
    pub title: Option<String>,
    /// 现在这一版的版本号
    pub current: Option<String>,
    /// 能装的最新版；`None` = 没有新版，或者压根认不出
    pub latest: Option<Latest>,
}

/// 可以装上去的那一版。
#[derive(Debug, Clone)]
pub struct Latest {
    pub version_number: String,
    pub file: modrinth::ModFile,
}

impl Check {
    /// 认出来了、而且确实有新版。
    pub fn is_update(&self) -> bool {
        self.latest.is_some()
    }

    /// 认得出这个文件属于哪个项目。
    pub fn is_known(&self) -> bool {
        self.title.is_some()
    }
}

/// 扫一遍这些 jar，问出哪些能更新。
///
/// `loaders` / `game_version` 是**这一份实例**的条件，用来筛「能装的新版」。
/// `on_progress` 收到 `(已完成, 总数, 当前在做什么)`。
pub fn scan<F>(
    files: &[PathBuf],
    loaders: &[String],
    game_version: &str,
    mut on_progress: F,
) -> Vec<Check>
where
    F: FnMut(usize, usize, &str),
{
    let total = files.len();
    let mut checks: Vec<Check> = files
        .iter()
        .map(|path| Check {
            path: path.clone(),
            filename: file_name(path),
            title: None,
            current: None,
            latest: None,
        })
        .collect();
    if total == 0 {
        return checks;
    }

    // 一、算哈希（纯磁盘活，不联网）
    on_progress(0, total, "计算文件指纹");
    let mut hashes: Vec<String> = Vec::with_capacity(total);
    let mut hash_owner: Vec<usize> = Vec::with_capacity(total);
    // 每个文件自己算出来的指纹，最后一步要用它判断「最新版是不是就是这个文件」
    let mut hash_at: HashMap<usize, String> = HashMap::new();
    for (index, check) in checks.iter().enumerate() {
        // 读不了的文件跳过，不拖垮整轮检查
        let Ok(sha1) = crate::sha1::sha1_file_hex(&check.path) else {
            continue;
        };
        hash_at.insert(index, sha1.clone());
        hashes.push(sha1);
        hash_owner.push(index);
    }

    // 二、批量反查哈希 → 项目 + 当前版本
    on_progress(0, total, "向 Modrinth 反查这些模组");
    let mut project_of: HashMap<usize, String> = HashMap::new();
    for (chunk_at, chunk) in hashes.chunks(BATCH).enumerate() {
        let body = format!(
            "{{\"hashes\":[{}],\"algorithm\":\"sha1\"}}",
            chunk
                .iter()
                .map(|h| format!("\"{}\"", h))
                .collect::<Vec<_>>()
                .join(",")
        );
        // 一批失败就跳过这一批：宁可认不出，也不要把已查到的作废
        let Ok(response) = crate::transport::post_json(&lookup_url(), &body) else {
            continue;
        };
        if !response.is_success() {
            continue;
        }
        let Ok(root) = json::parse(&response.body) else {
            continue;
        };
        for (offset, hash) in chunk.iter().enumerate() {
            let Some(item) = root.get(hash) else {
                continue;
            };
            let Some(project_id) = item.get("project_id").and_then(Json::as_str) else {
                continue;
            };
            let index = hash_owner[chunk_at * BATCH + offset];
            checks[index].current = item
                .get("version_number")
                .and_then(Json::as_str)
                .map(str::to_string);
            project_of.insert(index, project_id.to_string());
            if checks[index].title.is_none() {
                // 先拿项目 id 顶着，下面补到真名字再换
                checks[index].title = Some(project_id.to_string());
            }
        }
    }
    if project_of.is_empty() {
        on_progress(total, total, "没有认出任何来自 Modrinth 的模组");
        return checks;
    }

    // 三、补项目名（界面要显示名字，不是一串 id）
    let mut unique: Vec<String> = project_of.values().cloned().collect();
    unique.sort();
    unique.dedup();
    let titles = project_titles(&unique);
    for (index, id) in &project_of {
        if let Some(title) = titles.get(id) {
            checks[*index].title = Some(title.clone());
        }
    }

    // 四、逐个项目问「适合这份实例的最新版」，8 路并发
    //
    // 注意：Modrinth 的「最新」是「这个项目最近发布的那一版」，不等于
    // 「比你手上这版新」。同一个版本号重新打包、或者你装的就是最新版时，
    // 它照样会返回一条和现在一模一样的记录。所以这里必须自己比一次，
    // 否则界面上会把 39 个已经最新的模组全标成「有新版」。
    on_progress(0, total, "查询各项目的最新版");
    let results = fetch_latest(&unique, loaders, game_version);
    for (index, project_id) in &project_of {
        let Some(latest) = results.get(project_id) else {
            continue;
        };
        if is_newer(
            checks[*index].current.as_deref(),
            hash_at.get(index).map(String::as_str),
            latest,
        ) {
            checks[*index].latest = Some(latest.clone());
        }
    }

    on_progress(total, total, "检查完成");
    checks
}

/// 「这一版」和「现在盘上那个文件」不是同一份吗？
///
/// 有两道判据，任一命中都算「已经是最新」：
/// 1. 文件哈希一样 —— 同一个字节流，换了个文件名也不算更新；
/// 2. 版本号一样 —— 作者重新打过包（哈希变了）但版本号没动，那是修包不是升级。
fn is_newer(current_version: Option<&str>, current_hash: Option<&str>, latest: &Latest) -> bool {
    if let (Some(a), Some(b)) = (latest.file.sha1.as_deref(), current_hash) {
        if a.eq_ignore_ascii_case(b) {
            return false;
        }
    }
    !current_version.is_some_and(|cur| cur == latest.version_number)
}

fn lookup_url() -> String {
    format!("{}/version_files", MODRINTH_API)
}

fn projects_url(ids: &[String]) -> String {
    let list: Vec<String> = ids.iter().map(|id| format!("\"{}\"", id)).collect();
    format!(
        "{}/projects?ids={}",
        MODRINTH_API,
        crate::codec::urlencode(&format!("[{}]", list.join(",")))
    )
}

/// 项目 id → 项目名。
fn project_titles(ids: &[String]) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for chunk in ids.chunks(BATCH) {
        let Ok(text) = crate::transport::fetch_text(&projects_url(chunk)) else {
            continue;
        };
        let Ok(root) = json::parse(&text) else {
            continue;
        };
        let Some(list) = root.as_array() else {
            continue;
        };
        for item in list {
            let Some(id) = item.get("id").and_then(Json::as_str) else {
                continue;
            };
            let title = item
                .get("title")
                .and_then(Json::as_str)
                .unwrap_or(id)
                .to_string();
            out.insert(id.to_string(), title);
        }
    }
    out
}

/// 并发地把每个项目的最新版取回来。
fn fetch_latest(
    ids: &[String],
    loaders: &[String],
    game_version: &str,
) -> HashMap<String, Latest> {
    let mut out = HashMap::new();
    let workers = WORKERS.min(ids.len()).max(1);
    let chunks: Vec<&[String]> = if ids.len() <= workers {
        ids.chunks(1).collect()
    } else {
        // 连续分块：每个线程拿一段，末端那一两块少一点无所谓
        let per = ids.len().div_ceil(workers);
        ids.chunks(per.max(1)).collect()
    };

    std::thread::scope(|scope| {
        let handles: Vec<_> = chunks
            .into_iter()
            .map(|chunk| {
                scope.spawn(move || {
                    chunk
                        .iter()
                        .filter_map(|id| {
                            newest_for(id, loaders, game_version).map(|latest| (id.clone(), latest))
                        })
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        for handle in handles {
            if let Ok(pairs) = handle.join() {
                out.extend(pairs);
            }
        }
    });
    out
}

/// 这个项目在这份实例上能装的最新版。
///
/// 服务端已按发布时间倒序给，取第一条带可用文件的就是最新版。
fn newest_for(project_id: &str, loaders: &[String], game_version: &str) -> Option<Latest> {
    let mut url = format!(
        "{}/project/{}/version",
        MODRINTH_API,
        project_id
    );
    let mut query: Vec<String> = Vec::new();
    if !loaders.is_empty() {
        let list: Vec<String> = loaders.iter().map(|l| format!("\"{}\"", l)).collect();
        query.push(format!(
            "loaders={}",
            crate::codec::urlencode(&format!("[{}]", list.join(",")))
        ));
    }
    if !game_version.is_empty() {
        query.push(format!(
            "game_versions={}",
            crate::codec::urlencode(&format!("[\"{}\"]", game_version))
        ));
    }
    if !query.is_empty() {
        url.push('?');
        url.push_str(&query.join("&"));
    }
    let text = crate::transport::fetch_text(&url).ok()?;
    let list = modrinth::parse_versions(&text, ProjectKind::Mod).ok()?;
    let newest = list.into_iter().find(|v| v.file.is_some())?;
    Some(Latest {
        version_number: newest.version_number,
        file: newest.file?,
    })
}

/// 把新版换上：下载到临时名，成了再删旧的。
///
/// **先下后删**：下载失败时旧文件还在，这一份实例不会变成缺一个模组的样子。
/// 新旧文件名相同时（作者不常改文件名）直接覆盖，不走删除。
pub fn replace(old: &Path, latest: &Latest) -> Result<PathBuf, String> {
    let dir = old
        .parent()
        .ok_or_else(|| "找不到模组所在目录".to_string())?;
    let target = dir.join(&latest.file.filename);
    let same_name = target == old;

    crate::transport::download_to(&latest.file.url, &target, latest.file.sha1.as_deref())
        .map_err(|e| format!("下载 {} 失败: {}", latest.file.filename, e))?;

    if !same_name {
        // 新文件已经落好，这时候删旧的才安全
        if let Err(e) = std::fs::remove_file(old) {
            // 删不掉不算失败：新版能用，只是多留了一个旧 jar。
            // 但得让调用方知道，日志里要留一句。
            crate::logging::warn(format!(
                "{} 已装好，但旧的 {} 删不掉: {}",
                latest.file.filename,
                old.display(),
                e
            ));
        }
    }
    Ok(target)
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn latest(version: &str, filename: &str) -> Latest {
        Latest {
            version_number: version.to_string(),
            file: modrinth::ModFile {
                version_number: version.to_string(),
                url: "https://example.invalid/x.jar".to_string(),
                filename: filename.to_string(),
                sha1: None,
            },
        }
    }

    fn check(title: Option<&str>, latest: Option<Latest>) -> Check {
        Check {
            path: PathBuf::from("mods/a.jar"),
            filename: "a.jar".to_string(),
            title: title.map(str::to_string),
            current: title.map(|_| "0.5.0".to_string()),
            latest,
        }
    }

    #[test]
    fn a_known_jar_with_a_newer_version_is_an_update() {
        let c = check(Some("Sodium"), Some(latest("0.6.0", "sodium-0.6.0.jar")));
        assert!(c.is_update());
        assert!(c.is_known());
        assert_eq!(c.latest.as_ref().unwrap().version_number, "0.6.0");
    }

    #[test]
    fn an_unknown_jar_is_neither_known_nor_updatable() {
        let c = check(None, None);
        assert!(!c.is_update());
        assert!(!c.is_known());
    }

    #[test]
    fn a_known_jar_already_at_the_newest_is_not_an_update() {
        let c = check(Some("Sodium"), None);
        assert!(!c.is_update());
        assert!(c.is_known());
    }

    #[test]
    fn the_projects_url_encodes_the_id_list() {
        let url = projects_url(&["abc".to_string(), "def".to_string()]);
        // 方括号和引号都必须编码：curl 会拿它们当 shell 元字符
        assert!(url.contains("ids=%5B"), "{}", url);
        assert!(url.contains("abc"), "{}", url);
        assert!(!url.contains('"'), "{}", url);
    }

    fn with_sha1(version: &str, sha1: &str) -> Latest {
        let mut l = latest(version, "x.jar");
        l.file.sha1 = Some(sha1.to_string());
        l
    }

    #[test]
    fn the_same_file_is_never_an_update() {
        // 这篇实测打脸过：Modrinth 的「最新」是「最近发布的」，
        // 不是「比你这版新」，所以哈希一样时必须判成不用更新
        let l = with_sha1("26.3.159+fabric", "abc123");
        assert!(!is_newer(
            Some("26.3.159+fabric"),
            Some("ABC123"),
            &l
        ));
    }

    #[test]
    fn a_repack_with_the_same_number_is_not_an_update() {
        // 作者重新打包了（哈希变了），版本号没动：那是修包，不打扰玩家
        let l = with_sha1("1.46.0", "newhash");
        assert!(!is_newer(Some("1.46.0"), Some("oldhash"), &l));
    }

    #[test]
    fn a_different_version_is_an_update() {
        let l = with_sha1("1.83.2", "newhash");
        assert!(is_newer(Some("1.82.4"), Some("oldhash"), &l));
    }

    #[test]
    fn without_any_hash_we_fall_back_on_the_version_number() {
        let l = latest("2.0.0", "x.jar");
        assert!(is_newer(Some("1.0.0"), None, &l));
        assert!(!is_newer(Some("2.0.0"), None, &l));
        // 连当前版本都不知道：只能当它有新版，让玩家自己看
        assert!(is_newer(None, None, &l));
    }

    #[test]
    fn the_whole_scan_of_nothing_is_empty_not_an_error() {
        let checks = scan(&[], &["fabric".to_string()], "1.21.1", |_, _, _| {});
        assert!(checks.is_empty());
    }
}
