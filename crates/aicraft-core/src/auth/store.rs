//! 登录状态持久化。
//!
//! 位置：`<数据目录>/accounts.json`（默认在启动器 exe 旁的 `AICraftData` 里，
//! 与设置、日志同一个目录；写不进去才退到 `%APPDATA%\aicraft`）。
//! 进程重启后，启动器在这里找回账号，用 refresh token 续期，
//! 用户就不用每次都重新登。
//!
//! 关于**明文存储**：文件里确实有 refresh token。为了不引入 crate 依赖，
//! 这里暂时没有做 DPAPI 加密——`unsafe_code = "forbid"` 让我们没法直接调
//! 系统 API，只能像 TLS 那样借外部进程（PowerShell 的
//! `ProtectedData`）来做，那是下一步。当前的文件落在启动器自己的目录或
//! 用户自己的 `%APPDATA%` 下，NTFS 已经按用户隔离；但同一个 Windows 用户
//! 下的其他程序读得到，这一点写在这里，不装作不存在。

use std::path::{Path, PathBuf};

use crate::json::{self, Json};
use crate::launch::Account;

use super::json_escape;
use super::msa::MsaTokens;
use super::{AuthError, AuthErrorKind, Http, MinecraftAccount};

/// 文件格式版本。将来加字段/换加密方式时用它判断。
pub const STORE_VERSION: u32 = 1;

/// 一个已保存的账号。
#[derive(Debug, Clone, PartialEq)]
pub struct StoredAccount {
    /// 32 位无连字符 UUID，作为唯一键。
    pub uuid: String,
    /// 游戏内名字（即档案里的 `name`）。
    pub name: String,
    /// Minecraft 服务令牌，进游戏时用。
    pub access_token: String,
    /// 微软的 refresh token，用来在过期后续期；可能为空。
    pub refresh_token: Option<String>,
    /// Minecraft 令牌的过期时间（unix 秒）。
    pub expires_at: u64,
    /// 第三方登录的认证服务器根地址。
    ///
    /// 为 `Some` 时这是个皮肤站账号：启动要挂 authlib-injector，
    /// 续期也要回到这台服务器。`None` 是微软正版。
    pub api_root: Option<String>,
    /// 第三方登录的 clientToken（续期要与 accessToken 配对）。
    pub client_token: Option<String>,
    /// 是不是一个纯离线名字（没有令牌，也没有服务器）。
    ///
    /// 离线名字也进这个列表：玩家常常备着好几个名字来回用，
    /// 关掉启动器再打开不该只剩一个。
    pub offline: bool,
}

impl StoredAccount {
    /// 从一次成功的登录结果生成记录。
    pub fn from_login(account: &MinecraftAccount, tokens: &MsaTokens) -> StoredAccount {
        StoredAccount {
            uuid: account.uuid.clone(),
            name: account.username.clone(),
            access_token: account.access_token.clone(),
            refresh_token: tokens.refresh_token.clone(),
            expires_at: account.expires_at,
            api_root: None,
            client_token: None,
            offline: false,
        }
    }

    /// 是否已经过期（留 60 秒余量，避免卡在边界上）。
    ///
    /// `expires_at == 0` 表示没有过期时间（第三方服务器常常不给），
    /// 不能当成「1970 年就过期了」。
    pub fn is_expired(&self, now: u64) -> bool {
        self.expires_at != 0 && now + 60 >= self.expires_at
    }

    /// 从一次第三方登录的结果生成记录。
    pub fn from_yggdrasil(session: &super::yggdrasil::Session) -> StoredAccount {
        StoredAccount {
            uuid: session.uuid.clone(),
            name: session.name.clone(),
            access_token: session.access_token.clone(),
            refresh_token: None,
            // Yggdrasil 的令牌通常不过期，用 0 表示「不按时间判失效」，
            // 真失效了（改密码、被封）会在续期时拿到明确错误。
            expires_at: 0,
            api_root: Some(session.api_root.clone()),
            client_token: Some(session.client_token.clone()),
            offline: false,
        }
    }

    /// 一个纯离线的名字。
    ///
    /// UUID 走 Mojang 的 offline 规则（由名字派生）：同一个名字每次算出来
    /// 都一样，所以重复添加不会堆出两条。
    pub fn offline(name: &str) -> StoredAccount {
        let name = name.trim().to_string();
        StoredAccount {
            uuid: crate::launch::offline_uuid(&name),
            name,
            access_token: "0".repeat(32),
            refresh_token: None,
            expires_at: 0,
            api_root: None,
            client_token: None,
            offline: true,
        }
    }

    /// 是不是第三方（皮肤站）账号。
    pub fn is_third_party(&self) -> bool {
        self.api_root.is_some()
    }

    /// 能否自动续期。离线名字没有令牌，也就没有续期这回事。
    pub fn can_refresh(&self) -> bool {
        if self.offline {
            return false;
        }
        self.refresh_token.is_some() || self.api_root.is_some()
    }

    /// 转成启动器认识的账户结构。
    ///
    /// 第三方账号的 `user_type` 是 `mojang`：authlib-injector 靠它接管
    /// 认证流程，写 `msa` 会被游戏按微软流程判成无效令牌。
    pub fn to_account(&self) -> Account {
        if self.offline {
            return Account::offline(&self.name);
        }
        match &self.api_root {
            Some(root) => Account {
                username: self.name.clone(),
                uuid: self.uuid.clone(),
                access_token: self.access_token.clone(),
                user_type: "mojang".to_string(),
                demo: false,
                api_root: Some(root.clone()),
            },
            None => Account {
                username: self.name.clone(),
                uuid: self.uuid.clone(),
                access_token: self.access_token.clone(),
                user_type: "msa".to_string(),
                demo: false,
                api_root: None,
            },
        }
    }

    /// 用一次新的登录结果覆盖令牌字段。
    ///
    /// 微软会轮换 refresh token：新响应里带就换新的，没带就保留旧的——
    /// 直接覆盖成 `None` 会让下次续期无路可走。
    pub fn update_from(&mut self, account: &MinecraftAccount, tokens: &MsaTokens) {
        self.name = account.username.clone();
        self.access_token = account.access_token.clone();
        self.expires_at = account.expires_at;
        if let Some(refresh) = &tokens.refresh_token {
            self.refresh_token = Some(refresh.clone());
        }
    }

    /// 用保存的 refresh token 续期，并把新令牌写回自己。
    ///
    /// 启动前调用：Minecraft 令牌只有 24 小时，过期后进服会被踢。
    /// 成功时返回续期后的账户，调用方记得把这条记录存回账号文件。
    pub fn refresh(&mut self, http: &dyn Http, client_id: &str) -> Result<MinecraftAccount, AuthError> {
        self.refresh_with(http, client_id, |_| {})
    }

    /// 同 [`StoredAccount::refresh`]，但每进一跳先报一声。
    ///
    /// 启动前续期要跨五跳、可能好几秒，界面拿这些阶段画一条进度条。
    pub fn refresh_with<F>(
        &mut self,
        http: &dyn Http,
        client_id: &str,
        mut on_phase: F,
    ) -> Result<MinecraftAccount, AuthError>
    where
        F: FnMut(super::LoginPhase),
    {
        let Some(refresh_token) = self.refresh_token.clone() else {
            return Err(AuthError::new(
                AuthErrorKind::InvalidGrant,
                "账号文件里没有 refresh token，只能重新登录一次",
                String::new(),
            ));
        };

        let tokens = super::msa::refresh(http, client_id, &refresh_token)?;
        let account = super::complete_login_with(http, &tokens, &mut on_phase)?;
        self.update_from(&account, &tokens);
        Ok(account)
    }
}

/// 账号列表，按最近使用排序。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AccountStore {
    pub accounts: Vec<StoredAccount>,
    /// 上次用的是哪个账号（UUID）。空串表示没记过，按列表第一条算。
    ///
    /// 放在账号文件里而不是设置文件里：账号的事全放一处，
    /// 删账号时不会在别处留下一个指向不存在 UUID 的指针。
    pub last_uuid: String,
}

impl AccountStore {
    /// 读取账号文件。
    ///
    /// 文件不存在、内容坏掉都返回错误，由调用方决定是「当作空列表」
    /// 还是提示用户；这里不替调用方静默吞掉损坏的文件。
    ///
    /// 凭证是绑机器的（见 [`crate::seal`]）：换台电脑拷过来会解不开，
    /// 这时返回的错误里会写清楚「不是这台电脑」，而不是含糊的「格式错误」。
    pub fn load(path: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("读取 {} 失败：{}", path.display(), e))?;
        let plain = if crate::seal::looks_plain(&text) {
            // 老版本写的明文：照读，下次保存时自动变成封包
            text
        } else {
            let key = key_for(path)?;
            crate::seal::unseal(&text, &key)?
        };
        Self::from_json(&plain)
    }

    /// 读取；文件还不存在或读不了就当作空列表。
    pub fn load_or_default(path: &Path) -> Self {
        if path.is_file() {
            Self::load(path).unwrap_or_default()
        } else {
            AccountStore::default()
        }
    }

    pub fn from_json(text: &str) -> Result<Self, String> {
        let value = json::parse(text).map_err(|e| format!("账号文件不是合法 JSON：{}", e))?;
        let items = value
            .get("accounts")
            .and_then(|v| v.as_array())
            .ok_or_else(|| "账号文件里没有 accounts 数组".to_string())?;

        let mut accounts = Vec::with_capacity(items.len());
        for item in items {
            accounts.push(StoredAccount {
                uuid: str_field(item, "uuid")?,
                name: str_field(item, "name")?,
                access_token: str_field(item, "access_token")?,
                refresh_token: item
                    .get("refresh_token")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string()),
                expires_at: item
                    .get("expires_at")
                    .and_then(|v| v.as_i64())
                    .unwrap_or(0) as u64,
                api_root: item
                    .get("api_root")
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty())
                    .map(|s| s.to_string()),
                client_token: item
                    .get("client_token")
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty())
                    .map(|s| s.to_string()),
                offline: item
                    .get("offline")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false),
            });
        }

        let last_uuid = value
            .get("last_uuid")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        Ok(AccountStore {
            accounts,
            last_uuid,
        })
    }

    pub fn to_json(&self) -> String {
        let mut out = String::from("{\"version\":");
        out.push_str(&STORE_VERSION.to_string());
        if !self.last_uuid.is_empty() {
            out.push_str(",\"last_uuid\":\"");
            out.push_str(&json_escape(&self.last_uuid));
            out.push('"');
        }
        out.push_str(",\"accounts\":[");
        for (index, account) in self.accounts.iter().enumerate() {
            if index > 0 {
                out.push(',');
            }
            out.push_str("{\"uuid\":\"");
            out.push_str(&json_escape(&account.uuid));
            out.push_str("\",\"name\":\"");
            out.push_str(&json_escape(&account.name));
            out.push_str("\",\"access_token\":\"");
            out.push_str(&json_escape(&account.access_token));
            out.push('"');
            if let Some(refresh) = &account.refresh_token {
                out.push_str(",\"refresh_token\":\"");
                out.push_str(&json_escape(refresh));
                out.push('"');
            }
            out.push_str(",\"expires_at\":");
            out.push_str(&account.expires_at.to_string());
            if let Some(root) = &account.api_root {
                out.push_str(",\"api_root\":\"");
                out.push_str(&json_escape(root));
                out.push('"');
            }
            if let Some(token) = &account.client_token {
                out.push_str(",\"client_token\":\"");
                out.push_str(&json_escape(token));
                out.push('"');
            }
            // 只有真离线才写这个键：正版/第三方那两条不必带一串 false
            if account.offline {
                out.push_str(",\"offline\":true");
            }
            out.push('}');
        }
        out.push_str("]}");
        out
    }

    /// 保存到磁盘：先写临时文件再改名，避免中途断电留下半个 JSON。
    pub fn save(&self, path: &Path) -> Result<(), String> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("创建目录 {} 失败：{}", parent.display(), e))?;
        }

        let key = key_for(path)?;
        let sealed = crate::seal::seal(&self.to_json(), &key);

        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, sealed)
            .map_err(|e| format!("写入 {} 失败：{}", tmp.display(), e))?;
        std::fs::rename(&tmp, path).map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            format!("替换 {} 失败：{}", path.display(), e)
        })
    }

    /// 插入或更新一个账号，并把它挪到最前面（最近使用）。
    ///
    /// 同一个 UUID 视为同一个账号：换 token 时原地更新，不堆重复项。
    pub fn upsert(&mut self, account: StoredAccount) {
        self.last_uuid = account.uuid.clone();
        self.accounts.retain(|a| a.uuid != account.uuid);
        self.accounts.insert(0, account);
    }

    /// 当前该用哪个账号。
    ///
    /// 记着的那个找不到（被删了、文件被手改过）就退回列表第一条，
    /// 而不是报错——玩家想要的只是「能玩」，不是一份诊断报告。
    pub fn current(&self) -> Option<&StoredAccount> {
        if !self.last_uuid.is_empty() {
            if let Some(found) = self.find(&self.last_uuid) {
                return Some(found);
            }
        }
        self.accounts.first()
    }

    /// 切到某个账号：记下来，并挪到队首（列表按最近使用排序）。
    pub fn select(&mut self, uuid: &str) -> bool {
        if !self.accounts.iter().any(|a| a.uuid == uuid) {
            return false;
        }
        self.last_uuid = uuid.to_string();
        if let Some(index) = self.accounts.iter().position(|a| a.uuid == uuid) {
            let account = self.accounts.remove(index);
            self.accounts.insert(0, account);
        }
        true
    }

    pub fn find(&self, uuid: &str) -> Option<&StoredAccount> {
        self.accounts.iter().find(|a| a.uuid == uuid)
    }

    pub fn remove(&mut self, uuid: &str) -> bool {
        let before = self.accounts.len();
        self.accounts.retain(|a| a.uuid != uuid);
        let removed = before != self.accounts.len();
        if removed && self.last_uuid == uuid {
            // 删掉的正是上次用的那个：让 current() 自然回退到剩下的第一条
            self.last_uuid.clear();
        }
        removed
    }
}

/// 取这把钥匙：种子从数据目录读（没有就生成），再和机器指纹混一遍。
///
/// 种子跟着账号文件走（同一个数据目录），这样即使两台机器的指纹撞了，
/// 也不会因为共用同一个密钥而互相能解。换台电脑时种子可能跟着文件一起
/// 被拷过去，但机器指纹变了，算出来的钥匙就对不上。
fn key_for(path: &Path) -> Result<[u8; 32], String> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let seed = crate::seal::load_or_create_seed(dir)?;
    Ok(crate::seal::derive_key(&seed, crate::seal::machine_hint()))
}

/// 默认存储位置：`<账号目录>/accounts.json`。
///
/// 账号目录是 `%APPDATA%\aicraft`（见 [`crate::settings::accounts_dir`]），
/// 不跟着 exe 走：把启动器挪个文件夹不该等于登出。
pub fn default_path() -> PathBuf {
    crate::settings::accounts_dir().join("accounts.json")
}

fn str_field(item: &Json, name: &str) -> Result<String, String> {
    item.get(name)
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| format!("账号缺少字段 {}", name))
}

/// 当前 unix 时间戳（秒）。
pub fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> StoredAccount {
        StoredAccount {
            uuid: "uuid-1".to_string(),
            name: "Steve".to_string(),
            access_token: "mc-token".to_string(),
            refresh_token: Some("rt".to_string()),
            expires_at: 1_700_000_000,
            api_root: None,
            client_token: None,
            offline: false,
        }
    }

    #[test]
    fn third_party_accounts_survive_a_roundtrip() {
        let mut store = AccountStore::default();
        store.upsert(StoredAccount {
            uuid: "4f1e".to_string(),
            name: "皮肤站玩家".to_string(),
            access_token: "ys-token".to_string(),
            refresh_token: None,
            expires_at: 0,
            api_root: Some("https://littleskin.cn/api/yggdrasil".to_string()),
            client_token: Some("cli-1".to_string()),
            offline: false,
        });
        let back = AccountStore::from_json(&store.to_json()).unwrap();
        let account = &back.accounts[0];
        assert!(account.is_third_party());
        assert_eq!(account.client_token.as_deref(), Some("cli-1"));
        assert_eq!(
            account.api_root.as_deref(),
            Some("https://littleskin.cn/api/yggdrasil")
        );
        // user_type 必须是 mojang：authlib-injector 靠它接管认证
        assert_eq!(account.to_account().user_type, "mojang");
        // 没有过期时间的账号不算过期
        assert!(!account.is_expired(u64::MAX / 2));
        // 第三方账号即使没有 refresh_token 也能续期（走皮肤站自己的接口）
        assert!(account.can_refresh());
    }

    #[test]
    fn saved_files_are_sealed_not_plain() {
        let dir = crate::testutil::fresh_dir("store-seal");
        let path = dir.join("accounts.json");
        let mut store = AccountStore::default();
        store.upsert(StoredAccount::offline("星星"));
        store.save(&path).unwrap();

        let text = std::fs::read_to_string(&path).unwrap();
        // 明文里能看到名字；封包里看不到
        assert!(!text.contains("星星"));
        assert!(text.contains(crate::seal::SEAL_MARKER));

        let back = AccountStore::load(&path).unwrap();
        assert_eq!(back.accounts.len(), 1);
        assert_eq!(back.accounts[0].name, "星星");
        // 种子跟着账号文件落在同一个目录里
        assert!(dir.join(crate::seal::SEED_FILE).is_file());
    }

    #[test]
    fn a_file_from_another_machine_is_refused() {
        let dir = crate::testutil::fresh_dir("store-other");
        let path = dir.join("accounts.json");
        // 用「另一台机器」的指纹封一份，再按当前机器读
        let seed = [3u8; 32];
        std::fs::write(dir.join(crate::seal::SEED_FILE), "03".repeat(32)).unwrap();
        let other = crate::seal::derive_key(&seed, "another-computer|XYZ|other-profile");
        let mut store = AccountStore::default();
        store.upsert(StoredAccount::offline("星星"));
        std::fs::write(&path, crate::seal::seal(&store.to_json(), &other)).unwrap();

        let err = AccountStore::load(&path).unwrap_err();
        assert!(err.contains("不是这台电脑"), "实际是：{}", err);
    }

    #[test]
    fn an_offline_name_derives_the_same_uuid_every_time() {
        let a = StoredAccount::offline("星星");
        let b = StoredAccount::offline("星星");
        assert_eq!(a.uuid, b.uuid);
        assert!(a.offline);
        assert!(!a.can_refresh());
        // 转成启动账户后就是纯离线：legacy + 占位令牌
        let account = a.to_account();
        assert_eq!(account.user_type, "legacy");
        assert_eq!(account.access_token, "0".repeat(32));
        // 去掉首尾空格，名字不该带空白
        assert_eq!(StoredAccount::offline("  星星  ").name, "星星");
    }

    #[test]
    fn the_last_used_account_is_remembered_across_a_roundtrip() {
        let mut store = AccountStore::default();
        store.upsert(StoredAccount::offline("甲"));
        store.upsert(StoredAccount::offline("乙"));
        // upsert 把最近插入的挪到队首，也就是当前账号
        assert_eq!(store.current().map(|a| a.name.as_str()), Some("乙"));

        let back = AccountStore::from_json(&store.to_json()).unwrap();
        assert_eq!(back.last_uuid, store.last_uuid);
        assert_eq!(back.current().map(|a| a.name.as_str()), Some("乙"));
        // 离线标记也要存活
        assert!(back.accounts[0].offline);
    }

    #[test]
    fn selecting_an_account_moves_it_to_the_front() {
        let mut store = AccountStore::default();
        store.upsert(StoredAccount::offline("甲"));
        let target = store.accounts[0].uuid.clone();
        store.upsert(StoredAccount::offline("乙"));
        assert_eq!(store.current().map(|a| a.name.as_str()), Some("乙"));

        assert!(store.select(&target));
        assert_eq!(store.current().map(|a| a.name.as_str()), Some("甲"));
        assert_eq!(store.accounts[0].name, "甲");
        // 不认识的 UUID 不改变现状
        assert!(!store.select("nope"));
        assert_eq!(store.current().map(|a| a.name.as_str()), Some("甲"));
    }

    #[test]
    fn removing_the_current_account_falls_back_to_another() {
        let mut store = AccountStore::default();
        store.upsert(StoredAccount::offline("甲"));
        let first = store.accounts[0].uuid.clone();
        store.upsert(StoredAccount::offline("乙"));
        assert_eq!(store.current().map(|a| a.name.as_str()), Some("乙"));

        assert!(store.remove(&store.last_uuid.clone()));
        assert!(store.last_uuid.is_empty());
        assert_eq!(store.current().map(|a| a.name.as_str()), Some("甲"));
        assert_eq!(store.current().map(|a| a.uuid.as_str()), Some(first.as_str()));

        // 删光之后没有当前账号，但也不该 panic
        assert!(store.remove(&first));
        assert!(store.current().is_none());
    }

    #[test]
    fn microsoft_accounts_stay_msa() {
        let mut store = AccountStore::default();
        store.upsert(sample());
        let back = AccountStore::from_json(&store.to_json()).unwrap();
        let account = &back.accounts[0];
        assert!(!account.is_third_party());
        assert_eq!(account.to_account().user_type, "msa");
        assert!(account.is_expired(1_700_000_000));
        assert!(account.can_refresh());
    }

    fn temp_path(name: &str) -> PathBuf {
        // 用测试辅助而不是裸 %TEMP%：本机对临时目录的固定路径有写入限制，
        // 直接 create_dir_all 会拿到 PermissionDenied（见 src/testutil.rs）。
        crate::testutil::fresh_dir(&format!("store-{}", name)).join("accounts.json")
    }

    #[test]
    fn roundtrip_preserves_fields() {
        let mut store = AccountStore::default();
        store.upsert(sample());

        let text = store.to_json();
        let back = AccountStore::from_json(&text).unwrap();
        assert_eq!(back, store);
        assert_eq!(back.accounts[0].refresh_token.as_deref(), Some("rt"));
        assert_eq!(back.accounts[0].expires_at, 1_700_000_000);
    }

    #[test]
    fn missing_refresh_token_is_optional() {
        let account = StoredAccount {
            refresh_token: None,
            ..sample()
        };
        let mut store = AccountStore::default();
        store.upsert(account);
        let back = AccountStore::from_json(&store.to_json()).unwrap();
        assert!(back.accounts[0].refresh_token.is_none());
    }

    #[test]
    fn upsert_replaces_same_uuid_and_moves_to_front() {
        let mut store = AccountStore::default();
        store.upsert(StoredAccount {
            name: "Old".to_string(),
            ..sample()
        });
        store.upsert(StoredAccount {
            uuid: "uuid-2".to_string(),
            name: "Alex".to_string(),
            ..sample()
        });
        store.upsert(StoredAccount {
            name: "New".to_string(),
            ..sample()
        });

        assert_eq!(store.accounts.len(), 2);
        assert_eq!(store.accounts[0].name, "New");
        assert_eq!(store.accounts[1].uuid, "uuid-2");
    }

    #[test]
    fn save_and_load_on_disk() {
        let path = temp_path("disk");
        let mut store = AccountStore::default();
        store.upsert(sample());

        store.save(&path).unwrap();
        let loaded = AccountStore::load(&path).unwrap();
        assert_eq!(loaded, store);

        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn missing_file_is_empty_store() {
        let store = AccountStore::load_or_default(&temp_path("missing"));
        assert!(store.accounts.is_empty());
    }

    #[test]
    fn broken_json_is_an_error_not_a_panic() {
        assert!(AccountStore::from_json("{oops").is_err());
        assert!(AccountStore::from_json("{}").is_err());
        assert!(AccountStore::from_json(r#"{"accounts":[{"uuid":"u"}]}"#).is_err());
    }

    #[test]
    fn expiry_has_a_safety_margin() {
        let account = StoredAccount {
            expires_at: 1000,
            ..sample()
        };
        assert!(!account.is_expired(900));
        assert!(account.is_expired(940));
        assert!(account.is_expired(1000));
    }

    #[test]
    fn remove_deletes_only_that_account() {
        let mut store = AccountStore::default();
        store.upsert(sample());
        store.upsert(StoredAccount {
            uuid: "uuid-2".to_string(),
            ..sample()
        });
        assert!(store.remove("uuid-1"));
        assert!(!store.remove("uuid-1"));
        assert_eq!(store.accounts.len(), 1);
        assert_eq!(store.accounts[0].uuid, "uuid-2");
    }

    use crate::auth::testutil::MockHttp;
    use crate::transport::HttpResponse;

    fn ok(body: &str) -> HttpResponse {
        HttpResponse {
            status: 200,
            body: body.to_string(),
        }
    }

    /// 续期要走的五跳响应：微软令牌 → XBL → XSTS → Minecraft → 档案。
    fn refresh_responses(token_body: &str, mc_body: &str) -> MockHttp {
        MockHttp::new(vec![
            ok(token_body),
            ok(r#"{"Token":"xbl","DisplayClaims":{"xui":[{"uhs":"uhs-1"}]}}"#),
            ok(r#"{"Token":"xsts","DisplayClaims":{"xui":[{"uhs":"uhs-1"}]}}"#),
            ok(mc_body),
            ok(r#"{"id":"uuid-1","name":"Steve"}"#),
        ])
    }

    #[test]
    fn refresh_renews_tokens_and_rotates_refresh_token() {
        let mock = refresh_responses(
            r#"{"access_token":"msa-2","refresh_token":"rt-2","expires_in":3600}"#,
            r#"{"access_token":"mc-2","expires_in":86400}"#,
        );

        let mut account = StoredAccount {
            expires_at: 1,
            ..sample()
        };
        let renewed = account.refresh(&mock, "client-1").unwrap();

        assert_eq!(renewed.username, "Steve");
        assert_eq!(renewed.access_token, "mc-2");
        assert_eq!(account.access_token, "mc-2");
        assert_eq!(account.refresh_token.as_deref(), Some("rt-2"));
        assert!(account.expires_at > 1_700_000_000);
        assert_eq!(
            mock.log.borrow()[0].1,
            "https://login.microsoftonline.com/consumers/oauth2/v2.0/token"
        );
    }

    #[test]
    fn refresh_keeps_old_refresh_token_when_not_rotated() {
        let mock = refresh_responses(
            r#"{"access_token":"msa-2","expires_in":3600}"#,
            r#"{"access_token":"mc-2"}"#,
        );

        let mut account = sample();
        account.refresh(&mock, "client-1").unwrap();
        assert_eq!(account.refresh_token.as_deref(), Some("rt"));
    }

    #[test]
    fn refresh_without_refresh_token_is_invalid_grant() {
        let mock = MockHttp::new(Vec::new());
        let mut account = StoredAccount {
            refresh_token: None,
            ..sample()
        };
        let error = account.refresh(&mock, "client-1").unwrap_err();
        assert_eq!(error.kind, AuthErrorKind::InvalidGrant);
    }
}
