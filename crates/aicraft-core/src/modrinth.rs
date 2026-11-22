//! Modrinth 模组搜索与安装。
//!
//! 只依赖 `transport`（curl）和自家的 `json`：core 不引外部 crate，
//! 这里也不例外。搜索用的是公开的 v2 API，不需要 token，但**必须带
//! User-Agent** —— 不带会被 Cloudflare 挡成 400，`transport::fetch_text`
//! 已经把 `AICraftLauncher/0.1` 固定写进去了。
//!
//! 这一层只负责「把界面需要的字段取出来」和「把选中的文件放对位置」，
//! 不碰任何 egui 的东西 —— 方便单独测。

use std::path::{Path, PathBuf};

use crate::json::{self, Json};
use crate::loader::{LoaderKind, MODRINTH_API};

/// 搜索结果的排序方式（对应 API 的 `index` 参数）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sort {
    Relevance,
    Downloads,
    Follows,
    Newest,
    Updated,
}

impl Sort {
    pub const ALL: [Sort; 5] = [
        Sort::Relevance,
        Sort::Downloads,
        Sort::Follows,
        Sort::Newest,
        Sort::Updated,
    ];

    /// API 里的取值。
    pub fn id(self) -> &'static str {
        match self {
            Sort::Relevance => "relevance",
            Sort::Downloads => "downloads",
            Sort::Follows => "follows",
            Sort::Newest => "newest",
            Sort::Updated => "updated",
        }
    }

    /// 下拉框里显示的名字。
    pub fn label(self) -> &'static str {
        match self {
            Sort::Relevance => "相关度",
            Sort::Downloads => "下载量",
            Sort::Follows => "收藏数",
            Sort::Newest => "最新发布",
            Sort::Updated => "最近更新",
        }
    }
}

/// 资源下载能搜的四类东西。
///
/// Modrinth 用一个 `project_type` 区分它们（`mod` / `resourcepack` /
/// `shader` / `modpack`），四者能用的分类、落盘去向都不一样，所以这里
/// 一并带上。整合包是个例外：它不放进任何现有实例，而是装成一个新的，
/// 所以它没有「子目录」。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ProjectKind {
    #[default]
    Mod,
    ResourcePack,
    Shader,
    /// 数据包。Modrinth 没有独立的 `project_type`，但 facets 认
    /// `project_type:datapack`（它按项目的 `all_project_types` 筛），
    /// 站点上的目录也叫 `/datapack/<slug>`，所以 `id()` 照样能用。
    Datapack,
    Modpack,
}

impl ProjectKind {
    pub const ALL: [ProjectKind; 5] = [
        ProjectKind::Mod,
        ProjectKind::ResourcePack,
        ProjectKind::Shader,
        ProjectKind::Datapack,
        ProjectKind::Modpack,
    ];

    /// Modrinth 的 `project_type` 取值。
    pub fn id(self) -> &'static str {
        match self {
            ProjectKind::Mod => "mod",
            ProjectKind::ResourcePack => "resourcepack",
            ProjectKind::Shader => "shader",
            ProjectKind::Datapack => "datapack",
            ProjectKind::Modpack => "modpack",
        }
    }

    /// 侧边栏与标题上显示的名字。
    pub fn label(self) -> &'static str {
        match self {
            ProjectKind::Mod => "模组下载",
            ProjectKind::ResourcePack => "资源包下载",
            ProjectKind::Shader => "光影包下载",
            ProjectKind::Datapack => "数据包下载",
            ProjectKind::Modpack => "整合包下载",
        }
    }

    /// 单个东西的量词：一个模组 / 一个资源包 / 一个光影包 / 一个整合包。
    ///
    /// 「共 1234 个模组」里的「个」不是恒定的，「一个光影包」念着顺，
    /// 拼句子时直接用这个字更省事。
    pub fn unit(self) -> &'static str {
        match self {
            ProjectKind::Mod => "模组",
            ProjectKind::ResourcePack => "资源包",
            ProjectKind::Shader => "光影包",
            ProjectKind::Datapack => "数据包",
            ProjectKind::Modpack => "整合包",
        }
    }

    /// 这个扩展名的文件算不算这一类的东西。
    ///
    /// 模组只收 `.jar`；资源包与光影包只收 `.zip`（Modrinth 上它们就是
    /// 这么上传的）；整合包只收 `.mrpack`。宽松一点会把作者顺手传的
    /// `.mrpack`、`-sources.jar` 也列出来，点下去装进游戏的是个读不了的文件。
    pub fn accepts(self, filename: &str) -> bool {
        let name = filename.to_ascii_lowercase();
        match self {
            ProjectKind::Mod => name.ends_with(".jar"),
            // 数据包和资源包一样是 zip；同一个 Modrinth 版本里常同时有
            // 「数据包 zip」和「模组 jar」，收窄到 zip 才不会把 jar 当成数据包。
            ProjectKind::ResourcePack | ProjectKind::Shader | ProjectKind::Datapack => {
                name.ends_with(".zip")
            }
            ProjectKind::Modpack => name.ends_with(".mrpack"),
        }
    }

    /// 装进实例的哪个子目录；`None` 表示「不装进现有实例」。
    ///
    /// 整合包下载完之后走的是导入流程：新建一个实例再往里铺文件，
    /// 所以它没有可以塞进去的子目录。
    pub fn dir(self) -> Option<&'static str> {
        match self {
            ProjectKind::Mod => Some("mods"),
            ProjectKind::ResourcePack => Some("resourcepacks"),
            ProjectKind::Shader => Some("shaderpacks"),
            // 数据包不落在实例根：真正生效的位置是某个存档里的
            // `saves/<存档>/datapacks`，所以它跟整合包一样「不塞进实例根」，
            // 由调用方另外挑一个存档（见 `datapack_dir`）。
            ProjectKind::Datapack => None,
            ProjectKind::Modpack => None,
        }
    }

    /// 这一类是不是「挑一个存档，放进存档的 datapacks 里」。
    pub fn installs_into_world(self) -> bool {
        self == ProjectKind::Datapack
    }

    /// 这一类是不是「放进某个现有实例」的东西。
    ///
    /// 界面上靠它决定要不要摆「安装到」那个下拉框：整合包装成一个新实例，
    /// 让玩家挑「装进哪个」只会让他以为选错了地方。
    ///
    /// 数据包虽然不进实例根（dir() 是 None），但它确实要落在某个实例的
    /// saves/<存档>/datapacks 里，所以这里算「是」——它只是还要再挑存档。
    pub fn installs_into_instance(self) -> bool {
        self.dir().is_some() || self.installs_into_world()
    }

    /// 这一类的分类下拉表，第一项永远是「全部分类」。
    pub fn categories(self) -> &'static [(&'static str, &'static str)] {
        match self {
            ProjectKind::Mod => &MOD_CATEGORIES,
            ProjectKind::ResourcePack => &PACK_CATEGORIES,
            ProjectKind::Shader => &SHADER_CATEGORIES,
            ProjectKind::Datapack => &DATAPACK_CATEGORIES,
            ProjectKind::Modpack => &MODPACK_CATEGORIES,
        }
    }

    /// 只有模组按加载器筛。
    ///
    /// 资源包与光影包在 Modrinth 上不挂加载器 —— 它们挂的是 `minecraft`、
    /// `iris`、`optifine` 这类「谁能读它」，不是「给哪个加载器用」。
    /// 把加载器下拉摆在资源包页面只会筛出空列表。
    pub fn has_loader(self) -> bool {
        self == ProjectKind::Mod
    }
}

/// 查一个分类短名的中文名。三张表都翻，查不到返回 `None`。
pub fn category_label(id: &str) -> Option<&'static str> {
    let tables: [&[(&str, &str)]; 5] = [
        &MOD_CATEGORIES,
        &PACK_CATEGORIES,
        &SHADER_CATEGORIES,
        &DATAPACK_CATEGORIES,
        &MODPACK_CATEGORIES,
    ];
    for table in tables {
        if let Some((_, label)) = table.iter().find(|(key, _)| *key == id) {
            return Some(label);
        }
    }
    None
}

/// 模组分类。Modrinth 的类别是一串英文短名，界面上得说人话。
///
/// 这里只列常见的那些；没列到的分类仍然能靠搜索词命中，
/// 不会因为没进这张表就搜不到。
const MOD_CATEGORIES: [(&str, &str); 15] = [
    ("", "全部分类"),
    ("adventure", "冒险"),
    ("decoration", "装饰"),
    ("equipment", "装备"),
    ("food", "食物"),
    ("game-mechanics", "机制改动"),
    ("library", "前置库"),
    ("magic", "魔法"),
    ("optimization", "性能优化"),
    ("storage", "存储"),
    ("technology", "科技"),
    ("transportation", "交通"),
    ("utility", "实用工具"),
    ("worldgen", "世界生成"),
    ("cursed", "邪门"),
];

/// 资源包分类：先是分辨率（玩家最先看这个），再是它改变了什么。
const PACK_CATEGORIES: [(&str, &str); 22] = [
    ("", "全部分类"),
    ("8x-", "8x 及以下"),
    ("16x", "16x"),
    ("32x", "32x"),
    ("48x", "48x"),
    ("64x", "64x"),
    ("128x", "128x"),
    ("256x", "256x+"),
    ("512x+", "512x 及以上"),
    ("audio", "音效"),
    ("blocks", "方块"),
    ("core-shaders", "核心着色"),
    ("entities", "生物与实体"),
    ("environment", "环境"),
    ("equipment", "装备"),
    ("fonts", "字体"),
    ("gui", "界面"),
    ("items", "物品"),
    ("models", "模型"),
    ("tweaks", "细节调整"),
    ("realistic", "写实"),
    ("themed", "主题风格"),
];

/// 光影分类：先是画面特性，再是风格，最后是性能开销。
const SHADER_CATEGORIES: [(&str, &str); 20] = [
    ("", "全部分类"),
    ("atmosphere", "大气"),
    ("bloom", "泛光"),
    ("colored-lighting", "彩色光照"),
    ("foliage", "植被摇晃"),
    ("path-tracing", "路径追踪"),
    ("pbr", "PBR 材质支持"),
    ("reflections", "反射"),
    ("shadows", "阴影"),
    ("cartoon", "卡通"),
    ("fantasy", "奇幻"),
    ("realistic", "写实"),
    ("semi-realistic", "半写实"),
    ("vanilla-like", "原版风格"),
    ("cursed", "邪门"),
    ("low", "低开销"),
    ("medium", "中开销"),
    ("high", "高开销"),
    ("potato", "土豆机可跑"),
    ("screenshot", "截图专用"),
];

/// 数据包分类：玩法向的居多，分辨率那类完全用不上。
///
/// Modrinth 没有单独的 datapack 分类表（它的分类是按 `project_type` 分的），
/// 所以这张表是从模组/服务器那两张里挑出数据包真会用到的那部分——
/// 分类短名本身仍然由站点校验，挑错了只是筛不出东西。
const DATAPACK_CATEGORIES: [(&str, &str); 17] = [
    ("", "全部分类"),
    ("adventure", "冒险"),
    ("decoration", "装饰"),
    ("economy", "经济"),
    ("equipment", "装备"),
    ("food", "食物"),
    ("game-mechanics", "机制改动"),
    ("library", "前置库"),
    ("magic", "魔法"),
    ("mobs", "生物"),
    ("optimization", "性能优化"),
    ("social", "社交"),
    ("storage", "存储"),
    ("technology", "科技"),
    ("utilities", "实用工具"),
    ("worldgen", "世界生成"),
    ("cursed", "邪门"),
];

/// 整合包分类：按玩法流派分，和模组那套不完全重合。
///
/// 「多模组」「轻量」这类只在整合包上才说得通，所以单独一张表，
/// 不往模组那张里塞。
const MODPACK_CATEGORIES: [(&str, &str); 13] = [
    ("", "全部分类"),
    ("adventure", "冒险"),
    ("combat", "战斗"),
    ("kitchen-sink", "大杂烩"),
    ("lightweight", "轻量"),
    ("magic", "魔法"),
    ("multiplayer", "多人"),
    ("optimization", "性能优化"),
    ("quests", "任务线"),
    ("technology", "科技"),
    ("worldgen", "世界生成"),
    ("challenging", "硬核"),
    ("cursed", "邪门"),
];

/// 搜索命中的一个模组。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ModHit {
    /// `project_id`（安装时用它查版本）
    pub id: String,
    /// 短的 URL 名，界面上当副标题用
    pub slug: String,
    pub title: String,
    pub description: String,
    pub author: String,
    pub downloads: u64,
    pub follows: u64,
    /// 这个模组支持的游戏版本（API 只给最近的若干条）
    pub versions: Vec<String>,
    /// 支持它的加载器（`fabric` / `forge` / `neoforge`，来自 categories）
    pub loaders: Vec<String>,
    /// 图标地址。CDN 上多是 webp（少数 png / jpeg），解码交给界面层；
    /// 没有图标的老项目这里是空。
    pub icon_url: Option<String>,
}

impl ModHit {
    /// 下载量的口语写法：`12.3 万` 而不是 `123456`。
    ///
    /// 列表里几百个数字全写全，读的人反而比较不过来。
    pub fn downloads_text(&self) -> String {
        count_text(self.downloads)
    }
}

pub fn count_text(n: u64) -> String {
    if n >= 100_000_000 {
        format!("{:.1} 亿", n as f64 / 100_000_000.0)
    } else if n >= 10_000 {
        format!("{:.1} 万", n as f64 / 10_000.0)
    } else {
        n.to_string()
    }
}

/// 一次搜索的条件。
#[derive(Debug, Clone, Default)]
pub struct SearchQuery {
    /// 搜哪一类（模组 / 资源包 / 光影包）
    pub kind: ProjectKind,
    pub query: String,
    /// 空 = 不按游戏版本过滤
    pub game_version: Option<String>,
    /// `None` = 不按加载器过滤
    pub loader: Option<LoaderKind>,
    /// 分类短名，空串 = 全部分类
    pub category: Option<String>,
    pub sort: Option<Sort>,
    pub offset: usize,
    pub limit: usize,
}

impl SearchQuery {
    /// 拼出搜索 URL。
    ///
    /// facets 是「若干组条件，组内 OR、组间 AND」的二维数组，所以
    /// 加载器 / 分类 / 版本各占一组 —— 分类和加载器都是 Modrinth 的
    /// `categories` 字段，但必须分开写，否则会被当成「或」。
    pub fn url(&self) -> String {
        let mut facets: Vec<String> = vec![format!(r#"["project_type:{}"]"#, self.kind.id())];
        if let Some(loader) = self.loader.filter(|_| self.kind.has_loader()) {
            facets.push(format!(r#"["categories:{}"]"#, loader.id()));
        }
        if let Some(cat) = self.category.as_deref().filter(|c| !c.is_empty()) {
            facets.push(format!(r#"["categories:{}"]"#, cat));
        }
        if let Some(version) = self.game_version.as_deref().filter(|v| !v.is_empty()) {
            facets.push(format!(r#"["versions:{}"]"#, version));
        }
        let facet_json = format!("[{}]", facets.join(","));

        let sort = self.sort.unwrap_or(Sort::Relevance);
        let limit = if self.limit == 0 { 20 } else { self.limit };
        let mut url = format!(
            "{}/search?query={}&limit={}&offset={}&index={}",
            MODRINTH_API,
            crate::codec::urlencode(self.query.trim()),
            limit,
            self.offset,
            sort.id()
        );
        url.push_str("&facets=");
        url.push_str(&crate::codec::urlencode(&facet_json));
        url
    }
}

/// 解析一次搜索的响应。
///
/// 返回 `(命中的模组, 总命中数)` —— 总数用来画「共 N 个结果」和翻页。
pub fn parse_search(text: &str) -> Result<(Vec<ModHit>, u64), String> {
    let root = json::parse(text).map_err(|e| format!("解析搜索结果失败: {}", e))?;
    let hits = root
        .get("hits")
        .and_then(Json::as_array)
        .ok_or_else(|| "搜索结果里没有 hits".to_string())?;
    let total = root
        .get("total_hits")
        .and_then(Json::as_i64)
        .unwrap_or(hits.len() as i64)
        .max(0) as u64;

    let mut out = Vec::with_capacity(hits.len());
    for hit in hits {
        let Some(title) = hit.get("title").and_then(Json::as_str) else {
            continue;
        };
        let id = hit
            .get("project_id")
            .and_then(Json::as_str)
            .unwrap_or_default()
            .to_string();
        if id.is_empty() {
            continue;
        }
        let categories: Vec<String> = hit
            .get("categories")
            .and_then(Json::as_array)
            .map(|arr| {
                arr.iter()
                    .filter_map(|c| c.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        let loaders = categories
            .iter()
            .filter(|c| LoaderKind::from_id(c).is_some())
            .cloned()
            .collect();
        out.push(ModHit {
            icon_url: hit
                .get("icon_url")
                .and_then(Json::as_str)
                .filter(|u| !u.is_empty())
                .map(str::to_string),
            id,
            slug: hit
                .get("slug")
                .and_then(Json::as_str)
                .unwrap_or_default()
                .to_string(),
            title: title.to_string(),
            description: hit
                .get("description")
                .and_then(Json::as_str)
                .unwrap_or_default()
                .to_string(),
            author: hit
                .get("author")
                .and_then(Json::as_str)
                .unwrap_or_default()
                .to_string(),
            downloads: hit
                .get("downloads")
                .and_then(Json::as_i64)
                .unwrap_or(0)
                .max(0) as u64,
            follows: hit
                .get("follows")
                .and_then(Json::as_i64)
                .unwrap_or(0)
                .max(0) as u64,
            versions: hit
                .get("versions")
                .and_then(Json::as_array)
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default(),
            loaders,
        });
    }
    Ok((out, total))
}

/// 一个可下载的具体文件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModFile {
    /// 模组作者写的版本号，如 `mc1.21.1-0.6.13`
    pub version_number: String,
    pub url: String,
    pub filename: String,
    /// 服务端给的 SHA-1，用于下载后校验；老文件可能没有
    pub sha1: Option<String>,
}

/// 从一个版本条目里挑主文件。
///
/// 各收各的扩展名：模组是 `.jar`，资源包与光影包是 `.zip`。放宽了
/// 会把作者顺手传的 `-sources.jar`、`.mrpack` 也列出来，点下去装进
/// 游戏的是个读不了的文件。
fn primary_file(item: &Json, kind: ProjectKind) -> Option<ModFile> {
    let files = item.get("files").and_then(Json::as_array)?;
    let file = files
        .iter()
        .find(|f| f.get("primary").and_then(Json::as_bool).unwrap_or(false))
        .or_else(|| files.first())?;
    let url = file.get("url").and_then(Json::as_str)?;
    let filename = file
        .get("filename")
        .and_then(Json::as_str)
        .filter(|name| kind.accepts(name))?;
    Some(ModFile {
        version_number: item
            .get("version_number")
            .and_then(Json::as_str)
            .unwrap_or("未知版本")
            .to_string(),
        url: url.to_string(),
        filename: filename.to_string(),
        sha1: file
            .get("hashes")
            .and_then(|h| h.get("sha1"))
            .and_then(Json::as_str)
            .map(str::to_string),
    })
}

/// 把一个已经挑好的文件装进实例。
///
/// 「下哪一版」由调用方决定（详情页里玩家自己点的那一版），不再按
/// 加载器 / 版本去猜——列表页那个「一键下载最新兼容版」的按钮已经去掉了。
pub fn install_file(
    game_dir: &Path,
    instance: &str,
    kind: ProjectKind,
    file: &ModFile,
) -> Result<PathBuf, String> {
    let Some(sub) = kind.dir() else {
        return Err(format!(
            "{}不是放进实例的东西，它要装成一个新的",
            kind.unit()
        ));
    };
    let dir = content_dir(game_dir, instance, sub);
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("建 {} 目录失败（{}）: {}", sub, dir.display(), e))?;
    let dest = dir.join(&file.filename);
    crate::transport::download_to(&file.url, &dest, file.sha1.as_deref())
        .map_err(|e| format!("下载 {} 失败: {}", file.filename, e))?;
    Ok(dest)
}

/// 把一个整合包下载到暂存目录，返回落到盘上的路径。
///
/// 整合包不能像模组那样「下到 mods/ 里就完事」：它是整个实例的蓝图，
/// 下完还要读清单、确认实例名、铺文件。所以这里只负责把它取回来，
/// 放在 `<暂存目录>/整合包/` 下，交给导入流程接手。
///
/// 同名文件已经在了就覆盖：玩家点第二次通常是因为第一次没成，
/// 而不是想要两份一模一样的包。
pub fn download_pack(staging: &Path, file: &ModFile) -> Result<PathBuf, String> {
    let dir = staging.join("整合包");
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("建暂存目录失败（{}）: {}", dir.display(), e))?;
    let dest = dir.join(&file.filename);
    crate::transport::download_to(&file.url, &dest, file.sha1.as_deref())
        .map_err(|e| format!("下载 {} 失败: {}", file.filename, e))?;
    Ok(dest)
}

/// 导入成功之后，把暂存的那份整合包删掉。
///
/// 整合包动辄几百兆，装完还留在暂存目录里只是白占地方；同名再下会覆盖，
/// 所以删掉不丢东西。
///
/// 删不掉不算错：Windows 上文件被别的程序占着是常事，而导入本身已经成功了，
/// 不该因为收尾失败去报一个假错误。目录只在整个空了之后才收掉。
pub fn discard_pack(path: &Path) {
    let _ = std::fs::remove_file(path);
    if let Some(dir) = path.parent() {
        let _ = std::fs::remove_dir(dir);
    }
}

/// 模组详情页上要显示的东西。
///
/// 比 [`ModHit`] 多的是长描述（`body`，Modrinth 里是 markdown 原文）、
/// 分类全表、以及项目主页那几个链接。
#[derive(Debug, Clone, Default)]
pub struct ModDetail {
    pub id: String,
    /// 短的 URL 名。拼「在 Modrinth 上打开」的地址要用它，
    /// 比 id 好读，而且换了 id 也还是这个地址。
    pub slug: String,
    pub title: String,
    pub description: String,
    /// 长描述原文（markdown）。界面按纯文本显示，不做渲染——
    /// 为一个详情页塞进一个 markdown 渲染器不划算。
    pub body: String,
    pub author: String,
    pub downloads: u64,
    pub follows: u64,
    pub categories: Vec<String>,
    pub loaders: Vec<String>,
    /// 这个模组发布过的所有游戏版本（新的在前由服务端排好）
    pub game_versions: Vec<String>,
    pub license: String,
    pub source_url: Option<String>,
    pub issues_url: Option<String>,
    pub icon_url: Option<String>,
    /// `2024-05-01T12:00:00Z` 的日期部分
    pub updated: String,
}

impl ModDetail {
    pub fn downloads_text(&self) -> String {
        count_text(self.downloads)
    }

    pub fn follows_text(&self) -> String {
        count_text(self.follows)
    }
}

/// 模组的一个具体版本（详情页「其他版本」列表里的一行）。
#[derive(Debug, Clone)]
pub struct ModVersion {
    /// 版本 id，选中某个版本时用它回查（这里主要用来当界面 id）
    pub id: String,
    /// 作者给这一版起的名字，常为空
    pub name: String,
    pub version_number: String,
    pub game_versions: Vec<String>,
    pub loaders: Vec<String>,
    pub downloads: u64,
    /// 发布日期，只要 `YYYY-MM-DD`
    pub date: String,
    /// 这一版的主 `.jar`；不是 jar（整合包之类）时为空，界面不给装
    pub file: Option<ModFile>,
}

/// 详情请求的地址。
pub fn detail_url(project: &str) -> String {
    format!("{}/project/{}", MODRINTH_API, project)
}

/// 「所有版本」的地址。
///
/// 不带任何筛选：详情页要让玩家看到这个模组的全部版本，筛选在本地做——
/// 服务端过滤只能按一个版本 / 一个加载器，而列表里往往要「全部」。
pub fn versions_url(project: &str) -> String {
    format!("{}/project/{}/version", MODRINTH_API, project)
}

pub fn fetch_detail(project: &str) -> Result<String, String> {
    crate::transport::fetch_text(&detail_url(project))
        .map_err(|e| format!("取模组详情失败: {}", e))
}

pub fn fetch_versions(project: &str) -> Result<String, String> {
    crate::transport::fetch_text(&versions_url(project))
        .map_err(|e| format!("取模组版本列表失败: {}", e))
}

fn strings(item: &Json, key: &str) -> Vec<String> {
    item.get(key)
        .and_then(Json::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// 解析一个项目的详情。
pub fn parse_detail(text: &str) -> Result<ModDetail, String> {
    let root = json::parse(text).map_err(|e| format!("解析模组详情失败: {}", e))?;
    let id = root
        .get("id")
        .and_then(Json::as_str)
        .unwrap_or_default()
        .to_string();
    if id.is_empty() {
        return Err("模组详情里没有 id".to_string());
    }
    let categories = strings(&root, "categories");
    let loaders = root
        .get("loaders")
        .and_then(Json::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect::<Vec<_>>()
        })
        .unwrap_or_else(|| {
            categories
                .iter()
                .filter(|c| LoaderKind::from_id(c).is_some())
                .cloned()
                .collect()
        });
    Ok(ModDetail {
        slug: text_of(&root, "slug"),
        id,
        title: text_of(&root, "title"),
        description: text_of(&root, "description"),
        body: text_of(&root, "body"),
        updated: text_of(&root, "updated")
            .chars()
            .take(10)
            .collect::<String>(),
        author: root
            .get("team")
            .and_then(Json::as_str)
            .unwrap_or_default()
            .to_string(),
        downloads: root
            .get("downloads")
            .and_then(Json::as_i64)
            .unwrap_or(0)
            .max(0) as u64,
        follows: root
            .get("followers")
            .and_then(Json::as_i64)
            .unwrap_or(0)
            .max(0) as u64,
        categories,
        loaders,
        game_versions: strings(&root, "game_versions"),
        license: root
            .get("license")
            .and_then(|l| l.get("name"))
            .and_then(Json::as_str)
            .unwrap_or_default()
            .to_string(),
        source_url: root
            .get("source_url")
            .and_then(Json::as_str)
            .filter(|u| !u.is_empty())
            .map(str::to_string),
        issues_url: root
            .get("issues_url")
            .and_then(Json::as_str)
            .filter(|u| !u.is_empty())
            .map(str::to_string),
        icon_url: root
            .get("icon_url")
            .and_then(Json::as_str)
            .filter(|u| !u.is_empty())
            .map(str::to_string),
    })
}

/// 取一个字符串字段，缺了当空串。
fn text_of(root: &Json, key: &str) -> String {
    root.get(key)
        .and_then(Json::as_str)
        .unwrap_or_default()
        .to_string()
}

/// 解析「所有版本」列表。
///
/// 只留下能装的：没有 `.jar` 主文件的版本（整合包、数据包）在列表里点不动，
/// 还不如不显示。
pub fn parse_versions(text: &str, kind: ProjectKind) -> Result<Vec<ModVersion>, String> {
    let root = json::parse(text).map_err(|e| format!("解析模组版本失败: {}", e))?;
    let items = root
        .as_array()
        .ok_or_else(|| "模组版本列表不是数组".to_string())?;
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        let file = primary_file(item, kind);
        out.push(ModVersion {
            id: text_of(item, "id"),
            name: text_of(item, "name"),
            version_number: {
                let v = text_of(item, "version_number");
                if v.is_empty() {
                    "未知版本".to_string()
                } else {
                    v
                }
            },
            game_versions: strings(item, "game_versions"),
            loaders: strings(item, "loaders"),
            downloads: item
                .get("downloads")
                .and_then(Json::as_i64)
                .unwrap_or(0)
                .max(0) as u64,
            date: text_of(item, "date_published")
                .chars()
                .take(10)
                .collect::<String>(),
            file,
        });
    }
    Ok(out)
}

/// 实例的 `mods/` 目录。
///
/// 隔离实例放 `versions/<实例>/mods/`，共享实例放 `<游戏目录>/mods/` ——
/// 和游戏自己找模组的路径保持一致，放错地方等于没装。
pub fn mods_dir(game_dir: &Path, instance: &str) -> PathBuf {
    content_dir(game_dir, instance, "mods")
}

/// 实例里某个内容子目录（`mods` / `resourcepacks` / `shaderpacks`）。
///
/// 隔离实例放 `versions/<实例>/<子目录>/`，共享实例放 `<游戏目录>/<子目录>/`
/// —— 和游戏自己找东西的路径保持一致，放错地方等于没装。
pub fn content_dir(game_dir: &Path, instance: &str, sub: &str) -> PathBuf {
    match crate::instance::detect_mode(game_dir, instance) {
        crate::instance::InstanceMode::Isolated => {
            crate::instance::version_dir(game_dir, instance).join(sub)
        }
        crate::instance::InstanceMode::Shared => game_dir.join(sub),
    }
}

/// 一个实例下的所有存档名（`saves/` 里的子目录）。
///
/// 数据包只能塞进某个存档，所以界面得先把能选的存档列出来。
pub fn list_worlds(game_dir: &Path, instance: &str) -> Vec<String> {
    crate::instance::list_dirs(&content_dir(game_dir, instance, "saves"))
        .into_iter()
        .map(|item| item.name)
        .collect()
}

/// 某个存档的 `datapacks/` 目录。
///
/// 数据包真正的落点是**存档里**：`<实例>/saves/<存档>/datapacks/`。
/// 放在实例根的 `datapacks/` 原版游戏根本不读，所以这里必须带上存档名。
pub fn datapack_dir(game_dir: &Path, instance: &str, world: &str) -> PathBuf {
    content_dir(game_dir, instance, "saves")
        .join(world)
        .join("datapacks")
}

/// 把一个数据包装进指定存档。
///
/// 和 [`install_file`] 的区别只有落点：数据包进的是
/// `saves/<存档>/datapacks/`，不在实例根下。
pub fn install_datapack(
    game_dir: &Path,
    instance: &str,
    world: &str,
    file: &ModFile,
) -> Result<PathBuf, String> {
    if world.trim().is_empty() {
        return Err("还没有挑存档，数据包不知道往哪儿放".to_string());
    }
    let dir = datapack_dir(game_dir, instance, world);
    if !dir.is_dir() {
        return Err(format!(
            "{} 里没有这个存档（{}），先把游戏开一次让它生成",
            instance, world
        ));
    }
    let dest = dir.join(&file.filename);
    crate::transport::download_to(&file.url, &dest, file.sha1.as_deref())
        .map_err(|e| format!("下载 {} 失败: {}", file.filename, e))?;
    Ok(dest)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"{
        "hits": [
            {
                "project_id": "AANobbMI",
                "slug": "sodium",
                "title": "Sodium",
                "description": "现代渲染引擎",
                "author": "jellysquid3",
                "categories": ["optimization", "fabric"],
                "versions": ["1.21.1", "1.21"],
                "downloads": 12345678,
                "follows": 4242
            },
            {
                "title": "缺 project_id 的条目",
                "description": "应该被跳过"
            }
        ],
        "total_hits": 1234
    }"#;

    #[test]
    fn search_results_keep_what_the_ui_shows() {
        let (hits, total) = parse_search(SAMPLE).expect("样例应该能解析");
        assert_eq!(total, 1234);
        assert_eq!(hits.len(), 1, "没有 project_id 的条目会被跳过");
        let hit = &hits[0];
        assert_eq!(hit.id, "AANobbMI");
        assert_eq!(hit.title, "Sodium");
        assert_eq!(hit.author, "jellysquid3");
        assert_eq!(hit.downloads, 12_345_678);
        assert_eq!(hit.loaders, vec!["fabric".to_string()]);
        assert_eq!(hit.downloads_text(), "1234.6 万");
    }

    #[test]
    fn counts_read_naturally() {
        assert_eq!(count_text(0), "0");
        assert_eq!(count_text(9999), "9999");
        assert_eq!(count_text(10_000), "1.0 万");
        assert_eq!(count_text(250_000_000), "2.5 亿");
    }

    #[test]
    fn url_carries_every_filter() {
        let query = SearchQuery {
            kind: ProjectKind::Mod,
            query: "iris shaders".to_string(),
            game_version: Some("1.21.1".to_string()),
            loader: Some(LoaderKind::Fabric),
            category: Some("optimization".to_string()),
            sort: Some(Sort::Downloads),
            offset: 20,
            limit: 10,
        };
        let url = query.url();
        assert!(url.contains("query=iris%20shaders"), "空格要编码: {}", url);
        assert!(url.contains("limit=10"));
        assert!(url.contains("offset=20"));
        assert!(url.contains("index=downloads"));
        // facets 整体被编码，逗号与引号都变成 %XX
        assert!(url.contains("facets=%5B"), "{}", url);
        assert!(url.contains("project_type%3Amod"), "{}", url);
        assert!(url.contains("categories%3Afabric"), "{}", url);
        assert!(url.contains("categories%3Aoptimization"), "{}", url);
        assert!(url.contains("versions%3A1.21.1"), "{}", url);
    }

    #[test]
    fn loader_and_category_are_separate_groups() {
        // 同一个 facets 组里会被当成「或」，分类和加载器必须分组
        let query = SearchQuery {
            loader: Some(LoaderKind::Forge),
            category: Some("magic".to_string()),
            ..SearchQuery::default()
        };
        let raw = r#"[["project_type:mod"],["categories:forge"],["categories:magic"]]"#;
        assert!(query.url().contains(&crate::codec::urlencode(raw)));
    }

    #[test]
    fn each_kind_asks_for_its_own_project_type() {
        for kind in ProjectKind::ALL {
            let query = SearchQuery {
                kind,
                ..SearchQuery::default()
            };
            let url = query.url();
            let expected = format!(r#"[["project_type:{}"]]"#, kind.id());
            assert!(
                url.contains(&crate::codec::urlencode(&expected)),
                "{:?} 的 facets 不对: {}",
                kind,
                url
            );
        }
    }

    #[test]
    fn only_mods_carry_the_loader_facet() {
        // 资源包和光影包在 Modrinth 上不挂加载器，带上只会筛空
        let pack = SearchQuery {
            kind: ProjectKind::ResourcePack,
            loader: Some(LoaderKind::Fabric),
            ..SearchQuery::default()
        };
        assert!(
            !pack.url().contains("categories%3Afabric"),
            "资源包不该带加载器 facet: {}",
            pack.url()
        );
        let shader = SearchQuery {
            kind: ProjectKind::Shader,
            loader: Some(LoaderKind::Fabric),
            ..SearchQuery::default()
        };
        assert!(!shader.url().contains("categories%3Afabric"));
        // 模组照旧
        let modded = SearchQuery {
            kind: ProjectKind::Mod,
            loader: Some(LoaderKind::Fabric),
            ..SearchQuery::default()
        };
        assert!(modded.url().contains("categories%3Afabric"));
    }

    #[test]
    fn every_kind_lands_in_its_own_folder() {
        assert_eq!(ProjectKind::Mod.dir(), Some("mods"));
        assert_eq!(ProjectKind::ResourcePack.dir(), Some("resourcepacks"));
        assert_eq!(ProjectKind::Shader.dir(), Some("shaderpacks"));
        // 整合包不往现有实例里塞东西，它自己长成一个实例
        assert_eq!(ProjectKind::Modpack.dir(), None);
        assert!(!ProjectKind::Modpack.installs_into_instance());
        assert!(ProjectKind::Mod.installs_into_instance());
        // 数据包不进实例根：它只对某一个存档生效，落点得带上存档名
        assert_eq!(ProjectKind::Datapack.dir(), None);
        assert!(ProjectKind::Datapack.installs_into_instance());
        assert!(ProjectKind::Datapack.installs_into_world());
        assert!(!ProjectKind::Mod.installs_into_world());
        assert!(!ProjectKind::Modpack.installs_into_world());
    }

    #[test]
    fn datapack_categories_are_playability_oriented() {
        // 分辨率、加载器那类分类对数据包完全没意义
        assert!(!ProjectKind::Datapack
            .categories()
            .iter()
            .any(|(id, _)| *id == "16x" || *id == "fabric"));
        assert!(ProjectKind::Datapack
            .categories()
            .iter()
            .any(|(id, _)| *id == "worldgen"));
        assert_eq!(category_label("worldgen"), Some("世界生成"));
    }

    fn sample_file() -> ModFile {
        ModFile {
            version_number: "1.0".to_string(),
            url: "http://127.0.0.1:1/never.zip".to_string(),
            filename: "terralith.zip".to_string(),
            sha1: None,
        }
    }

    #[test]
    fn a_datapack_without_a_world_is_refused_before_touching_the_disk() {
        let temp = crate::testutil::fresh_dir("modrinth-noworld");
        let err = install_datapack(&temp, "1.20.1", "  ", &sample_file())
            .expect_err("没挑存档就该拦下");
        assert!(err.contains("存档"), "{}", err);
    }

    #[test]
    fn datapacks_land_inside_the_chosen_world() {
        let temp = crate::testutil::fresh_dir("modrinth-world");
        // 存档目录不在时必须明说，而不是自己新建一个空存档
        let err = install_datapack(&temp, "1.20.1", "world", &sample_file())
            .expect_err("存档不存在就该拒绝");
        assert!(err.contains("没有这个存档"), "{}", err);
        assert!(
            !datapack_dir(&temp, "1.20.1", "world").exists(),
            "不该顺手把存档目录造出来"
        );

        let dir = datapack_dir(&temp, "1.20.1", "world");
        assert!(dir.ends_with("world/datapacks"), "{}", dir.display());
    }

    #[test]
    fn worlds_are_listed_straight_from_the_saves_folder() {
        let temp = crate::testutil::fresh_dir("modrinth-saves");
        assert!(list_worlds(&temp, "1.20.1").is_empty(), "还没有存档");
        let saves = content_dir(&temp, "1.20.1", "saves");
        std::fs::create_dir_all(saves.join("New World")).expect("建存档");
        let worlds = list_worlds(&temp, "1.20.1");
        assert_eq!(worlds, vec!["New World".to_string()]);
    }

    #[test]
    fn modpacks_take_mrpack_only() {
        assert!(ProjectKind::Modpack.accepts("Fabulously Optimized 5.9.0.mrpack"));
        assert!(!ProjectKind::Modpack.accepts("Faithful 32x.zip"));
        assert!(!ProjectKind::Modpack.accepts("sodium.jar"));
        // 反过来：.mrpack 不该被当成资源包或光影包
        assert!(!ProjectKind::ResourcePack.accepts("pack.mrpack"));
        assert!(!ProjectKind::Shader.accepts("pack.mrpack"));
        assert!(!ProjectKind::Mod.accepts("pack.mrpack"));
    }

    #[test]
    fn the_modpack_tab_has_its_own_categories() {
        assert_eq!(ProjectKind::Modpack.categories()[0], ("", "全部分类"));
        assert!(ProjectKind::Modpack
            .categories()
            .iter()
            .any(|(id, _)| *id == "kitchen-sink"));
        // 「大杂烩」只在整合包上有意义
        assert!(!ProjectKind::Mod
            .categories()
            .iter()
            .any(|(id, _)| *id == "kitchen-sink"));
        // 查分类名要能翻到第四张表
        assert_eq!(category_label("kitchen-sink"), Some("大杂烩"));
    }

    #[test]
    fn a_pack_lands_in_the_staging_folder() {
        let root = crate::testutil::fresh_dir("packstage");
        let src = root.join("src");
        std::fs::create_dir_all(&src).expect("建源目录");
        // 文件名不带空格：file:// 地址里的空格 curl 会当成畸形 URL 拒掉，
        // 真实的 Modrinth 地址是 %20 编码过的，不受这个影响。
        let archive = src.join("Fabulously-Optimized.mrpack");
        std::fs::write(&archive, b"not really a zip").expect("写源文件");
        // curl 认 file://，这里不必真去网上拉一个几百兆的包
        let url = format!(
            "file:///{}",
            archive.display().to_string().replace('\\', "/")
        );
        let file = ModFile {
            version_number: "1.0".to_string(),
            url,
            filename: "Fabulously-Optimized.mrpack".to_string(),
            sha1: None,
        };
        let staging = root.join("downloads");
        let dest = download_pack(&staging, &file).expect("该能下到暂存目录");
        assert!(dest.is_file(), "{}", dest.display());
        assert_eq!(
            dest.parent(),
            Some(staging.join("整合包").as_path()),
            "整合包要单独放一个子目录，别和别的东西混在一起"
        );
        assert_eq!(
            dest.file_name().and_then(|n| n.to_str()),
            Some("Fabulously-Optimized.mrpack")
        );
    }

    #[test]
    fn a_pack_leaves_nothing_behind_once_it_is_installed() {
        let root = crate::testutil::fresh_dir("packdiscard");
        let dir = root.join("downloads").join("整合包");
        std::fs::create_dir_all(&dir).expect("建暂存目录");
        let dest = dir.join("Fabulously-Optimized.mrpack");
        std::fs::write(&dest, b"zip").expect("写包");
        discard_pack(&dest);
        assert!(!dest.exists(), "装完之后包不该还占着地方");
        assert!(!dir.exists(), "整个目录空了，连目录一起收掉");
    }

    #[test]
    fn discarding_a_pack_that_is_already_gone_is_quiet() {
        let root = crate::testutil::fresh_dir("packdiscard2");
        // 文件本来就不在、父目录也不在，都不该 panic：收尾失败不是错。
        discard_pack(&root.join("downloads").join("整合包").join("gone.mrpack"));
    }

    #[test]
    fn installing_a_pack_into_an_instance_is_refused() {
        let file = ModFile {
            version_number: "1.0".to_string(),
            url: "https://example.invalid/pack.mrpack".to_string(),
            filename: "pack.mrpack".to_string(),
            sha1: None,
        };
        let err = install_file(
            Path::new("E:/nowhere"),
            "1.20.1-Fabric",
            ProjectKind::Modpack,
            &file,
        )
        .expect_err("整合包不该被塞进实例");
        assert!(err.contains("整合包"), "{}", err);
    }

    #[test]
    fn each_kind_starts_with_all_categories() {
        for kind in ProjectKind::ALL {
            assert_eq!(kind.categories()[0], ("", "全部分类"), "{:?}", kind);
        }
        // 三张表不共用：「16x」只在资源包里有
        assert!(ProjectKind::ResourcePack
            .categories()
            .iter()
            .any(|(id, _)| *id == "16x"));
        assert!(!ProjectKind::Mod.categories().iter().any(|(id, _)| *id == "16x"));
        assert!(ProjectKind::Shader
            .categories()
            .iter()
            .any(|(id, _)| *id == "path-tracing"));
    }

    #[test]
    fn empty_filters_only_keep_project_type() {
        let url = SearchQuery::default().url();
        assert!(url.contains(&crate::codec::urlencode(r#"[["project_type:mod"]]"#)));
        assert!(url.contains("index=relevance"));
        assert!(url.contains("limit=20"));
    }

    #[test]
    fn packs_take_zip_while_mods_take_jar() {
        let text = r#"[
            {
                "version_number": "26.3",
                "files": [
                    {"url": "https://x/Faithful%2032x.zip", "filename": "Faithful 32x - 26.3.zip",
                     "primary": true, "hashes": {"sha1": "zip"}}
                ]
            }
        ]"#;
        // 资源包：zip 收下
        let packs = parse_versions(text, ProjectKind::ResourcePack).expect("能解析");
        assert_eq!(packs[0].file.as_ref().map(|f| f.filename.as_str()), Some("Faithful 32x - 26.3.zip"));
        // 模组：同一份清单里的 zip 不算模组，文件留空（界面上显示「无文件」）
        let mods = parse_versions(text, ProjectKind::Mod).expect("能解析");
        assert!(mods[0].file.is_none(), "zip 不该被当成模组");
        // 光影包同样收 zip
        let shaders = parse_versions(text, ProjectKind::Shader).expect("能解析");
        assert!(shaders[0].file.is_some());
    }

    #[test]
    fn a_version_without_the_right_extension_is_kept_but_fileless() {
        let text = r#"[
            {"version_number": "1.0", "files": [{"url": "https://x/a.zip", "filename": "a.zip"}]}
        ]"#;
        let versions = parse_versions(text, ProjectKind::Mod).expect("能解析");
        // 版本本身留着（玩家还能看到它存在），只是没有可下载文件
        assert_eq!(versions.len(), 1);
        assert_eq!(versions[0].version_number, "1.0");
        assert!(versions[0].file.is_none());
    }

    #[test]
    fn primary_file_wins_over_the_first_one() {
        let text = r#"[
            {
                "version_number": "1.0",
                "files": [
                    {"url": "https://x/sources.jar", "filename": "mod-sources.jar"},
                    {"url": "https://x/mod.jar", "filename": "mod.jar", "primary": true,
                     "hashes": {"sha1": "abc"}}
                ]
            }
        ]"#;
        let versions = parse_versions(text, ProjectKind::Mod).expect("样例应该能解析");
        let file = versions[0].file.as_ref().expect("应该挑到主文件");
        assert_eq!(file.filename, "mod.jar");
        assert_eq!(file.sha1.as_deref(), Some("abc"));
        assert_eq!(file.version_number, "1.0");
    }

    #[test]
    fn non_jar_files_are_skipped() {
        // 整合包是 .mrpack，放进 mods 是垃圾文件。
        // 版本条目本身还留着（让人知道发过这版），只是没有可装的 jar。
        let text = r#"[
            {"version_number": "2.0", "files": [{"url": "https://x/pack.mrpack", "filename": "pack.mrpack", "primary": true}]}
        ]"#;
        let versions = parse_versions(text, ProjectKind::Mod).expect("样例应该能解析");
        assert_eq!(versions.len(), 1);
        assert!(versions[0].file.is_none());
    }

    #[test]
    fn searching_nothing_is_an_error_not_a_silent_success() {
        assert!(parse_search("{}").is_err());
    }

    // 用 r##"…"##：正文里 `"# Sodium` 那串会把 r#"…"# 提前收尾。
    const DETAIL: &str = r##"{
        "id": "AANobbMI",
        "slug": "sodium",
        "title": "Sodium",
        "description": "现代渲染引擎",
        "body": "# Sodium

快的渲染。",
        "team": "CaffeineMC",
        "downloads": 45678,
        "followers": 1234,
        "categories": ["optimization", "fabric"],
        "loaders": ["fabric"],
        "game_versions": ["1.21.1", "1.21"],
        "license": {"id": "lgpl-3", "name": "LGPL-3.0-only"},
        "source_url": "https://github.com/CaffeineMC/sodium",
        "issues_url": "",
        "icon_url": "https://cdn.modrinth.com/data/AANobbMI.png",
        "updated": "2024-05-01T12:00:00Z"
    }"##;

    #[test]
    fn detail_reads_what_the_page_shows() {
        let d = parse_detail(DETAIL).expect("样例应该能解析");
        assert_eq!(d.id, "AANobbMI");
        assert_eq!(d.slug, "sodium");
        assert_eq!(d.author, "CaffeineMC");
        assert_eq!(d.follows, 1234);
        assert_eq!(d.follows_text(), "1234");
        assert_eq!(d.license, "LGPL-3.0-only");
        assert_eq!(d.updated, "2024-05-01");
        assert_eq!(d.loaders, vec!["fabric".to_string()]);
        assert_eq!(d.source_url.as_deref(), Some("https://github.com/CaffeineMC/sodium"));
        // 空串的链接当没有：界面上不画一个点了没反应的按钮
        assert_eq!(d.issues_url, None);
        assert!(d.body.contains("Sodium"));
    }

    #[test]
    fn detail_without_id_is_rejected() {
        assert!(parse_detail(r#"{"title": "没有 id"}"#).is_err());
    }

    #[test]
    fn versions_keep_the_installable_ones() {
        let text = r#"[
            {
                "id": "v1",
                "name": "Release 1",
                "version_number": "1.2.3",
                "game_versions": ["1.21.1"],
                "loaders": ["fabric"],
                "downloads": 999,
                "date_published": "2024-04-02T10:00:00Z",
                "files": [{"url": "https://x/mod.jar", "filename": "mod.jar", "primary": true,
                           "hashes": {"sha1": "abc"}}]
            },
            {
                "id": "v2",
                "version_number": "1.2.2",
                "game_versions": ["1.21"],
                "loaders": ["fabric"],
                "date_published": "2024-03-01T10:00:00Z",
                "files": [{"url": "https://x/pack.mrpack", "filename": "pack.mrpack", "primary": true}]
            }
        ]"#;
        let versions = parse_versions(text, ProjectKind::Mod).expect("样例应该能解析");
        assert_eq!(versions.len(), 2);
        assert_eq!(versions[0].version_number, "1.2.3");
        assert_eq!(versions[0].date, "2024-04-02");
        assert!(versions[0].file.is_some());
        // 整合包那一条保留（让人知道发过这版），但没有可装的 jar
        assert!(versions[1].file.is_none());
    }

    #[test]
    fn versions_must_be_an_array() {
        assert!(parse_versions("{}", ProjectKind::Mod).is_err());
    }
}
