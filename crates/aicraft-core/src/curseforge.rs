//! CurseForge 整合包的文件清单。
//!
//! CurseForge 的整合包（`manifest.json`）不写下载地址，只写「哪个项目的哪个
//! 文件」——`projectID` + `fileID`。要拿到真正能下的 URL，得拿这两个数去查
//! 一次 API。
//!
//! **为什么走镜像**：官方 `api.curseforge.com` 一律要 API key（不带就是 403），
//! 而 key 不该硬编进一个开源启动器里——那等于替所有人共用一份配额，用光了
//! 大家都没得玩。`mod.mcimirror.top` 提供的是同一套 v1 接口的无 key 转发，
//! 字段名、路径、分页格式都对得上。真拿到 key 了换 [`API`] 一个常量就行。
//!
//! 下载地址本身来自 `forgecdn.net`，和 API 无关，所以镜像只承担「查」这一步。

use crate::json::{self, Json};
use crate::natives::ZipEntry;

/// CurseForge v1 接口入口（无 key 转发）。
pub const API: &str = "https://mod.mcimirror.top/curseforge/v1";

/// 清单文件名：见到它就是 CurseForge 包。
pub const CURSEFORGE_MANIFEST: &str = "manifest.json";

/// `manifest.json` 里的一条文件引用。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManifestFile {
    pub project_id: u64,
    pub file_id: u64,
    /// 清单里有时写着「作者要求必装」，这一版先不看
    pub required: bool,
}

/// CurseForge 整合包解析出来的信息。
#[derive(Debug, Clone)]
pub struct Manifest {
    pub name: String,
    pub version: String,
    /// `minecraft.version`
    pub game_version: String,
    /// `minecraft.modLoaders` 里认出来的加载器
    pub loaders: Vec<String>,
    pub author: String,
    pub files: Vec<ManifestFile>,
}

/// 这个压缩包是不是 CurseForge 整合包。
///
/// 只靠文件名不够：CurseForge 的 `manifest.json` 和别的格式重名过，
/// 所以要**读进来**看看有没有 `minecraft` + `manifestType` 这两个键。
/// 判断逻辑放在 [`parse_manifest`] 里，这里只挑候选。
pub fn is_manifest_name(name: &str) -> bool {
    name.trim_start_matches("./") == CURSEFORGE_MANIFEST
}

/// 解析 `manifest.json`。
///
/// 返回 `Ok(None)` 表示「这个 JSON 不是 CurseForge 格式」，让调用方继续
/// 试别的；`Err` 才是真的坏了。
pub fn parse_manifest(text: &str) -> Result<Option<Manifest>, String> {
    let root = match json::parse(text) {
        Ok(root) => root,
        // 不是合法 JSON 时不该在这里报错——调用方可能正拿着别的格式的
        // manifest.json 在试。交给上层判断，这里当「不是」。
        Err(_) => return Ok(None),
    };
    let Some(kind) = root.get("manifestType").and_then(Json::as_str) else {
        return Ok(None);
    };
    if !kind.eq_ignore_ascii_case("minecraftmodpack") {
        return Ok(None);
    }

    let minecraft = root.get("minecraft");
    let game_version = minecraft
        .and_then(|m| m.get("version"))
        .and_then(Json::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "CurseForge 清单里没写 Minecraft 版本".to_string())?
        .to_string();

    let loaders = minecraft
        .and_then(|m| m.get("modLoaders"))
        .and_then(Json::as_array)
        .map(parse_loaders)
        .unwrap_or_default();

    let mut files = Vec::new();
    if let Some(list) = root.get("files").and_then(Json::as_array) {
        for item in list {
            let (Some(project_id), Some(file_id)) = (
                item.get("projectID").and_then(Json::as_i64),
                item.get("fileID").and_then(Json::as_i64),
            ) else {
                continue;
            };
            if project_id <= 0 || file_id <= 0 {
                continue;
            }
            files.push(ManifestFile {
                project_id: project_id as u64,
                file_id: file_id as u64,
                required: item
                    .get("required")
                    .and_then(Json::as_bool)
                    .unwrap_or(true),
            });
        }
    }

    let name = root
        .get("name")
        .and_then(Json::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("未命名整合包")
        .to_string();
    let version = root
        .get("version")
        .and_then(Json::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("未知版本")
        .to_string();
    let author = root
        .get("author")
        .and_then(Json::as_str)
        .unwrap_or_default()
        .to_string();

    Ok(Some(Manifest {
        name,
        version,
        game_version,
        loaders,
        author,
        files,
    }))
}

/// `modLoaders` 里每条形如 `{"id":"fabric-0.15.11","primary":true}`。
///
/// 返回的是**原始 id**（`fabric-0.15.11`），因为要不要装、装哪个版本由
/// `modpack` 那边统一判断（它已经有 `parse_loader_id`）。
fn parse_loaders(list: &[Json]) -> Vec<String> {
    let mut out = Vec::new();
    for item in list {
        let Some(id) = item.get("id").and_then(Json::as_str) else {
            continue;
        };
        let id = id.trim();
        if !id.is_empty() {
            out.push(id.to_string());
        }
    }
    out
}

/// 一个已经查好、可以直接下的文件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedFile {
    /// 这个文件属于哪个项目（下载得到的文件名常常看不出，日志里要写）
    pub project_id: u64,
    pub file_id: u64,
    pub filename: String,
    pub url: String,
    pub size: u64,
    /// `hashes` 里 `algo == 1` 的那条，就是 SHA-1
    pub sha1: Option<String>,
    /// 项目叫什么（查不到时为空，只是少一行字）
    pub project_name: String,
    /// 项目的分类 id（`classId`）。它决定文件该落在哪个目录：
    /// 12 = 资源包、6945 = 数据包、6552 = 光影，其余按模组算。
    pub class_id: u64,
}

/// 查一批文件。
///
/// 分两趟：先用 `POST /mods/files` 一次问回全部文件的下载地址（CurseForge
/// 一次最多认 100 个 fileID），再对没查到的项目用 `POST /mods` 补名字。
/// 分两趟是因为第一个接口不回项目名，而列表里「哪个 mod 没下成」得说清。
pub fn resolve_files(files: &[ManifestFile]) -> Result<Vec<ResolvedFile>, String> {
    if files.is_empty() {
        return Ok(Vec::new());
    }

    let mut out = Vec::with_capacity(files.len());
    for chunk in files.chunks(MAX_BATCH) {
        let ids: Vec<String> = chunk.iter().map(|f| f.file_id.to_string()).collect();
        let body = format!(r#"{{"fileIds":[{}]}}"#, ids.join(","));
        let resp = crate::transport::post_json(&format!("{}/mods/files", API), &body)
            .map_err(|e| format!("连不上 CurseForge 镜像: {}", e))?;

        // 批量接口对「有一个 id 不存在」的反应是整批 404，而不是少回一条。
        // 一个早就被删掉的老文件就能把整包堵死，所以这里退化成逐个查：
        // 单条查不到只会少一条，其余照常。
        if !resp.is_success() {
            for file in chunk {
                if let Some(found) = resolve_one(file)? {
                    out.push(found);
                }
            }
            continue;
        }

        let root = json::parse(&resp.body)
            .map_err(|e| format!("解析 CurseForge 文件列表失败: {}", e))?;
        let data = root
            .get("data")
            .and_then(Json::as_array)
            .ok_or_else(|| "CurseForge 响应里没有 data".to_string())?;
        for item in data {
            let Some(found) = resolved_from(item) else {
                continue;
            };
            out.push(found);
        }
    }

    // 清单里点名了、但接口没回的文件（作者删了、或者项目被隐藏）。
    // 这种不能悄悄跳过——玩家会以为装完了，进游戏才发现少东西。
    let got: Vec<u64> = out.iter().map(|f| f.file_id).collect();
    let missing: Vec<&ManifestFile> = files
        .iter()
        .filter(|f| f.required && !got.contains(&f.file_id))
        .collect();

    fill_names(&mut out, files)?;

    if !missing.is_empty() {
        let ids: Vec<String> = missing
            .iter()
            .map(|f| format!("{}/{}", f.project_id, f.file_id))
            .collect();
        return Err(format!(
            "整合包里有 {} 个文件在 CurseForge 上找不到了：{}",
            missing.len(),
            ids.join("、")
        ));
    }
    Ok(out)
}

/// 一次能问多少个 fileID。官方文档写的是 100，镜像沿用。
const MAX_BATCH: usize = 100;

/// CurseForge 的分类 id → 落盘目录。
///
/// CurseForge 的清单只写「哪个项目的哪个文件」，不写它是什么东西；光看文件名
/// 也认不出（`.zip` 可能是资源包、数据包或光影）。所以查项目详情时顺手记下
/// `classId`，按它分目录——和 HMCL 的 `guessFilePath` 同一张表。
pub fn target_dir(class_id: u64) -> &'static str {
    match class_id {
        12 => "resourcepacks",
        6945 => "datapacks",
        6552 => "shaderpacks",
        _ => "mods",
    }
}

/// 单独查一个文件；查不到返回 `Ok(None)`。
///
/// 走 `GET /mods/{project}/files/{file}`：这个接口不存在的 id 会老实回 404，
/// 而不是像批量接口那样把一整批带下水。
fn resolve_one(file: &ManifestFile) -> Result<Option<ResolvedFile>, String> {
    let url = format!("{}/mods/{}/files/{}", API, file.project_id, file.file_id);
    let resp = crate::transport::get_with_headers(&url, &[])
        .map_err(|e| format!("连不上 CurseForge 镜像: {}", e))?;
    if resp.status == 404 {
        return Ok(None);
    }
    if !resp.is_success() {
        return Err(format!("CurseForge 返回 {}（查文件）", resp.status));
    }
    let root = json::parse(&resp.body)
        .map_err(|e| format!("解析 CurseForge 文件失败: {}", e))?;
    Ok(root.get("data").and_then(resolved_from))
}

fn resolved_from(item: &Json) -> Option<ResolvedFile> {
    let file_id = item.get("id").and_then(Json::as_i64)?;
    let mod_id = item.get("modId").and_then(Json::as_i64).unwrap_or(0);
    let filename = item
        .get("fileName")
        .and_then(Json::as_str)
        .unwrap_or_default()
        .to_string();
    // `downloadUrl` 为空 = 作者关掉了「第三方分发」这个开关。CurseForge 的
    // CDN 路径是可拼的（`files/<id/1000>/<id%1000>/<文件名>`），实测这种文件
    // 在 CDN 上照样给 200，所以退一步直拼——HMCL 也是这么做的
    // （`CurseForgeRemoteAddonRepository.LatestFile.downloadUrl()`）。
    // 拼不出来（没有文件名）才留空，让上层报「要你手动去下」。
    let url = match item.get("downloadUrl").and_then(Json::as_str) {
        Some(url) if !url.is_empty() => url.to_string(),
        _ if !filename.is_empty() => format!(
            "https://edge.forgecdn.net/files/{}/{}/{}",
            file_id / 1000,
            file_id % 1000,
            filename
        ),
        _ => String::new(),
    };
    let size = item
        .get("fileLength")
        .and_then(Json::as_i64)
        .unwrap_or(0)
        .max(0) as u64;
    let sha1 = item
        .get("hashes")
        .and_then(Json::as_array)
        .and_then(|list| {
            list.iter()
                .find(|h| h.get("algo").and_then(Json::as_i64) == Some(1))
                .and_then(|h| h.get("value"))
                .and_then(Json::as_str)
        })
        .map(str::to_string);

    Some(ResolvedFile {
        project_id: mod_id.max(0) as u64,
        file_id: file_id.max(0) as u64,
        filename,
        url,
        size,
        sha1,
        project_name: String::new(),
        class_id: 0,
    })
}

/// 给每个文件补上项目名（失败不算错：只是列表里少一行字）。
fn fill_names(out: &mut [ResolvedFile], files: &[ManifestFile]) -> Result<(), String> {
    // 文件里没带 modId 的，用清单里的 projectID 补
    for file in out.iter_mut() {
        if file.project_id == 0 {
            if let Some(found) = files.iter().find(|f| f.file_id == file.file_id) {
                file.project_id = found.project_id;
            }
        }
    }
    let mut ids: Vec<u64> = out.iter().map(|f| f.project_id).filter(|i| *i > 0).collect();
    ids.sort_unstable();
    ids.dedup();
    if ids.is_empty() {
        return Ok(());
    }

    for chunk in ids.chunks(MAX_BATCH) {
        let list: Vec<String> = chunk.iter().map(|i| i.to_string()).collect();
        let body = format!(r#"{{"modIds":[{}]}}"#, list.join(","));
        let Ok(resp) = crate::transport::post_json(&format!("{}/mods", API), &body) else {
            return Ok(());
        };
        if !resp.is_success() {
            return Ok(());
        }
        let Ok(root) = json::parse(&resp.body) else {
            return Ok(());
        };
        let Some(data) = root.get("data").and_then(Json::as_array) else {
            return Ok(());
        };
        for item in data {
            let Some(id) = item.get("id").and_then(Json::as_i64) else {
                continue;
            };
            let Some(name) = item.get("name").and_then(Json::as_str) else {
                continue;
            };
            // 分类决定文件落哪个目录：12 资源包、6945 数据包、6552 光影、
            // 其余（6 / 4471 等）按模组。HMCL 用的是同一张表
            // （`CurseCompletionTask.guessFilePath`）。
            let class_id = item
                .get("classId")
                .and_then(Json::as_i64)
                .unwrap_or(0)
                .max(0) as u64;
            for file in out.iter_mut() {
                if file.project_id == id.max(0) as u64 {
                    file.project_name = name.to_string();
                    file.class_id = class_id;
                }
            }
        }
    }
    Ok(())
}

/// CurseForge 整合包里 `overrides/` 就是实例内容（和 Modrinth 同名同义）。
pub fn overrides_prefix() -> &'static [&'static str] {
    &["overrides/"]
}

/// `overrides/` 之外该忽略的顶层文件（清单本身不进实例目录）。
pub fn skipped(entries: &[ZipEntry]) -> Vec<String> {
    let _ = entries;
    vec![CURSEFORGE_MANIFEST.to_string()]
}

#[cfg(test)]
mod tests {
    use super::*;

    const MANIFEST: &str = r##"{
        "minecraft": {
            "version": "1.21.1",
            "modLoaders": [
                { "id": "fabric-0.15.11", "primary": true }
            ]
        },
        "manifestType": "minecraftModpack",
        "manifestVersion": 1,
        "name": "All the Mods 10",
        "version": "1.0.0",
        "author": "ATMTeam",
        "files": [
            { "projectID": 394468, "fileID": 8756581, "required": true },
            { "projectID": 238222, "fileID": 1, "required": false }
        ]
    }"##;

    #[test]
    fn a_curseforge_manifest_reads_what_the_wizard_needs() {
        let found = parse_manifest(MANIFEST).expect("应该能解析").expect("应该是 CF 包");
        assert_eq!(found.name, "All the Mods 10");
        assert_eq!(found.version, "1.0.0");
        assert_eq!(found.game_version, "1.21.1");
        assert_eq!(found.author, "ATMTeam");
        assert_eq!(found.loaders, vec!["fabric-0.15.11"]);
        assert_eq!(found.files.len(), 2);
        assert_eq!(found.files[0].project_id, 394468);
        assert_eq!(found.files[0].file_id, 8756581);
        assert!(!found.files[1].required);
    }

    #[test]
    fn someone_elses_manifest_is_not_a_curseforge_pack() {
        // 没有 manifestType：可能是别的启动器的同名文件
        let text = r#"{"minecraft": {"version": "1.20.1"}, "files": []}"#;
        assert!(parse_manifest(text).unwrap().is_none());
        // 坏 JSON 也不该报错——调用方还在试别的格式
        assert!(parse_manifest("{").unwrap().is_none());
        // manifestType 对但类型不对
        let other = r#"{"manifestType": "somethingElse", "minecraft": {"version": "1.20.1"}}"#;
        assert!(parse_manifest(other).unwrap().is_none());
    }

    #[test]
    fn a_manifest_without_a_game_version_is_an_error() {
        let text = r#"{"manifestType": "minecraftModpack", "minecraft": {}}"#;
        assert!(parse_manifest(text).is_err());
    }

    #[test]
    fn files_without_ids_are_skipped_instead_of_crashing() {
        let text = r##"{
            "manifestType": "minecraftModpack",
            "minecraft": { "version": "1.21.1" },
            "files": [
                { "projectID": 0, "fileID": 5 },
                { "projectID": 7, "fileID": 0 },
                { "nothing": true },
                { "projectID": 9, "fileID": 11 }
            ]
        }"##;
        let found = parse_manifest(text).unwrap().unwrap();
        assert_eq!(found.files.len(), 1);
        assert_eq!(found.files[0].project_id, 9);
    }

    #[test]
    fn each_class_gets_its_own_folder() {
        // 12 资源包、6945 数据包、6552 光影，其余按模组
        assert_eq!(target_dir(12), "resourcepacks");
        assert_eq!(target_dir(6945), "datapacks");
        assert_eq!(target_dir(6552), "shaderpacks");
        assert_eq!(target_dir(6), "mods");
        assert_eq!(target_dir(4471), "mods");
        assert_eq!(target_dir(0), "mods");
    }

    #[test]
    fn only_the_top_level_manifest_is_recognised() {
        assert!(is_manifest_name("manifest.json"));
        assert!(is_manifest_name("./manifest.json"));
        assert!(!is_manifest_name("overrides/manifest.json"));
        assert!(!is_manifest_name("modrinth.index.json"));
    }
}
