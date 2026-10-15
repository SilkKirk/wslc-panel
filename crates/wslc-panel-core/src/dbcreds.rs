//! 数据库账号的**本地凭据存档**（`db-credentials.json`）。
//!
//! # 为什么要把密码写到磁盘上
//!
//! v0.8 起「创建数据库」可以**同时创建用户**，而列表里要展示
//! "这个库的用户名 / 密码"。MySQL 的密码是单向哈希 —— 建完就查不回来了，
//! 所以面板必须自己记。1Panel 也是这么做的（它同样在自己的库存明文）。
//!
//! # 存什么、存哪、风险讲清
//!
//! - 文件：`%LOCALAPPDATA%\wslc-panel\db-credentials.json`（和 `prefs.json`
//!   同一个目录，见 [`crate::prefs`]）；
//! - 结构：`{ "<容器名>/<库名>": { "user": …, "password": …, "host": "%" } }`
//!   —— **键里带容器名**：两个容器各有一个 `shop` 库时互不串台；
//! - ⚠️ **明文**。这是"要在列表里展示密码"这个产品决定的直接代价，
//!   换任何编码（base64 之类）都只是自欺 —— 写在这里，不藏着。
//!   面板进程能被读到内存的攻击者，也能读到这份文件，威胁模型没变。
//!
//! # 兜底
//!
//! 解析失败（手改坏了 / 半截写入）**不 panic、不覆盖**：返回空档并在
//! 读取时把原文留给调用方提示。写入走"临时文件 + 重命名"，
//! 半途断电最多丢掉这一次的记录，不会把旧档写坏。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// 一个库的账号记录。
///
/// ⚠️ `Debug` **手写脱敏**（密码打 `***`）：它会被 `DbState` / `AppState`
/// 派生的 `Debug` 一路带出来 —— 存的是明文是一回事，把它打进**日志**是另一回事。
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DbCredential {
    /// MySQL 用户名（默认 `<库名>_user`）。
    pub user: String,
    /// 密码（创建时生成 / 用户手改的那份）。
    pub password: String,
    /// 允许连接的主机（`%` = 任意）。
    pub host: String,
}

impl std::fmt::Debug for DbCredential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DbCredential")
            .field("user", &self.user)
            .field(
                "password",
                &if self.password.is_empty() {
                    "<empty>"
                } else {
                    "***"
                },
            )
            .field("host", &self.host)
            .finish()
    }
}

/// 全部记录，按 `"<容器名>/<库名>"` 索引。
///
/// `BTreeMap`：序列化出来是**按键排序**的 —— 用户手改这个文件时能看懂，
/// diff 也稳定。
///
/// `Debug` 派生安全：值类型 [`DbCredential`] 自己已经脱敏
/// （键是容器名 / 库名，不含密码）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DbCredentialStore {
    #[serde(flatten)]
    entries: BTreeMap<String, DbCredential>,
}

/// `"<容器>/<库>"` 形式的键。
///
/// 单独一个函数：容器名里理论上可以有 `/`（`wslc` 不允许，但解析层的原则是
/// 不 panic）—— 拼的时候小心一点，读的时候按**最后一次**出现的 `/` 拆。
pub fn key(container: &str, database: &str) -> String {
    format!("{}/{}", container.trim(), database.trim())
}

impl DbCredentialStore {
    /// 读档。文件不存在 → 空档（第一次运行就是这么进来的）；
    /// 解析失败 → 空档 + `Some(原因)`（调用方决定要不要提示）。
    pub fn load(path: &Path) -> (Self, Option<String>) {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return (Self::default(), None),
            Err(e) => return (Self::default(), Some(format!("读不到凭据存档：{e}"))),
        };
        match serde_json::from_str::<DbCredentialStore>(&text) {
            Ok(store) => (store, None),
            Err(e) => (
                Self::default(),
                Some(format!(
                    "凭据存档解析失败（{}）—— 里面的账号要重新录入：{e}",
                    path.display()
                )),
            ),
        }
    }

    /// 写档（临时文件 + 重命名，见模块注释）。
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let text = serde_json::to_string_pretty(self).unwrap_or_else(|_| "{}".to_owned());
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, text)?;
        // rename 覆盖已存在的目标：Windows 上 `std::fs::rename` 对已存在目标
        // 会失败（POSIX 不会），所以先删一次 —— 两步之间断电的最坏情况是
        // "旧档还在 / 新档在 .tmp"，都不会把档写坏。
        if path.exists() {
            std::fs::remove_file(path)?;
        }
        std::fs::rename(&tmp, path)
    }

    /// 取一个库的账号。
    pub fn get(&self, container: &str, database: &str) -> Option<&DbCredential> {
        self.entries.get(&key(container, database))
    }

    /// 记 / 更新一个库的账号。
    pub fn put(
        &mut self,
        container: &str,
        database: &str,
        credential: DbCredential,
    ) -> Option<DbCredential> {
        self.entries.insert(key(container, database), credential)
    }

    /// 删掉一个库的账号（删库时一起清）。
    pub fn remove(&mut self, container: &str, database: &str) -> Option<DbCredential> {
        self.entries.remove(&key(container, database))
    }

    /// 条数（测试与"共几条"的展示用）。
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// 是否为空。
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// 默认的存档路径：`<数据目录>/db-credentials.json`。
///
/// `data_dir` 传 `prefs.json` 所在的目录（`%LOCALAPPDATA%\wslc-panel`），
/// 保持"面板自己的东西都在一处"。
pub fn store_path(data_dir: impl AsRef<Path>) -> PathBuf {
    data_dir.as_ref().join("db-credentials.json")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cred(user: &str, password: &str) -> DbCredential {
        DbCredential {
            user: user.to_owned(),
            password: password.to_owned(),
            host: "%".to_owned(),
        }
    }

    #[test]
    fn keys_are_scoped_by_container() {
        let mut store = DbCredentialStore::default();
        store.put("mysql", "shop", cred("shop_user", "aaa"));
        store.put("mysql2", "shop", cred("shop_user", "bbb"));
        // 两个容器各有一个 `shop` —— 互不串台（这正是键里带容器名的原因）
        assert_eq!(store.get("mysql", "shop").unwrap().password, "aaa");
        assert_eq!(store.get("mysql2", "shop").unwrap().password, "bbb");
        assert_eq!(store.len(), 2);
        assert_eq!(key(" mysql ", " shop "), "mysql/shop");
    }

    #[test]
    fn round_trips_through_disk() {
        let dir = std::env::temp_dir().join(format!("wslc-dbcreds-{}", std::process::id()));
        let path = store_path(&dir);
        let _ = std::fs::remove_file(&path);

        let mut store = DbCredentialStore::default();
        store.put("mysql", "shop", cred("shop_user", "s3cret"));
        store.save(&path).expect("应能写档");

        let (loaded, error) = DbCredentialStore::load(&path);
        assert!(error.is_none(), "{error:?}");
        assert_eq!(loaded.get("mysql", "shop").unwrap().password, "s3cret");
        assert_eq!(loaded.len(), 1);

        std::fs::remove_file(&path).ok();
        std::fs::remove_dir(&dir).ok();
    }

    #[test]
    fn missing_file_is_an_empty_store_not_an_error() {
        let (store, error) = DbCredentialStore::load(Path::new(
            r"Z:\definitely\not\a\real\path\db-credentials.json",
        ));
        assert!(store.is_empty());
        assert!(error.is_none(), "文件不存在不算错：{error:?}");
    }

    #[test]
    fn corrupt_file_degrades_to_empty_and_reports() {
        let dir = std::env::temp_dir().join(format!("wslc-dbcreds-bad-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = store_path(&dir);
        std::fs::write(&path, "{ 不是 json").unwrap();

        let (store, error) = DbCredentialStore::load(&path);
        assert!(store.is_empty());
        assert!(error.is_some(), "损坏要能被说出来");

        std::fs::remove_file(&path).ok();
        std::fs::remove_dir(&dir).ok();
    }

    #[test]
    fn remove_drops_exactly_one_entry() {
        let mut store = DbCredentialStore::default();
        store.put("mysql", "shop", cred("shop_user", "a"));
        store.put("mysql", "crm", cred("crm_user", "b"));
        assert_eq!(store.remove("mysql", "shop").unwrap().user, "shop_user");
        assert!(store.get("mysql", "shop").is_none());
        assert!(store.get("mysql", "crm").is_some());
        assert_eq!(store.remove("mysql", "shop"), None);
    }

    #[test]
    fn debug_never_shows_the_password() {
        // ⚠️ 存的是明文（产品决定），但**日志**里不能有 ——
        // `DbState` / `AppState` 派生的 Debug 会一路打到这里。
        let mut store = DbCredentialStore::default();
        store.put("mysql", "shop", cred("shop_user", "s3cret"));
        let text = format!("{store:?}");
        assert!(!text.contains("s3cret"), "{text}");
        assert!(text.contains("***"), "{text}");
        assert!(text.contains("shop_user"), "{text}");
    }
}
