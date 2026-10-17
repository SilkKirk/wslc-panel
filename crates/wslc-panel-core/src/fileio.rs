//! 面板自己那几个 JSON 存档的**原子写**与崩溃恢复。
//!
//! # 为什么需要它（v0.9.20）
//!
//! `dbcreds::save` / `deploys::save` 早先都是同一种写法：
//!
//! ```text
//! 写 .tmp → 删掉目标 → rename(.tmp, 目标)
//! ```
//!
//! 注释说"两步之间断电最坏是旧档还在"，但**旧档已经在上一行被删了**。
//! 如果删完、rename 之前进程崩溃（或断电），结果是：旧档没了，新档还在
//! `.tmp`，下次启动读到"文件不存在" → 空存档。部署记录和数据库账号
//! 都会这么丢。
//!
//! 现在改成三步，任何一步之后崩溃都能读回**至少一份完整数据**：
//!
//! 1. 写 `.tmp`；
//! 2. 旧档先 rename 成 `.bak`（同目录 rename 是原子的）；
//! 3. `.tmp` rename 成主文件，成功后删掉 `.bak`。
//!
//! 崩溃点与恢复：
//!
//! | 崩在哪 | 磁盘上 | [`read_text`] 的行为 |
//! |---|---|---|
//! | 写完 `.tmp` | 主文件还是旧的 | 读旧的（`.tmp` 被忽略） |
//! | 旧档已变 `.bak` | 主文件不存在、`.bak` 是旧的 | 读 `.bak` 并放回主文件 |
//! | 新档已就位 | 主文件是新的、`.bak` 是旧的 | 读新的，顺手清掉 `.bak` |
//!
//! ⚠️ 不引入 `tempfile` / `windows` 之类的依赖：本机没有 Rust 工具链，
//! 改 `Cargo.toml` 会动 `Cargo.lock`（见 `AGENTS.md` §3）。`std::fs`
//! 的 rename 在同一目录内足够完成这件事。

use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};

/// 主文件旁边的 `.bak` 路径。
///
/// 用 `OsString` 追加后缀而不是 `Path::with_extension`：后者会把
/// `deployments.json` 变成 `deployments.json.bak` 没问题，但对没有扩展名
/// 的路径会吃掉最后一段。追加字符串对所有路径形状都一致。
pub fn backup_path(path: &Path) -> PathBuf {
    sidecar(path, ".bak")
}

fn tmp_path(path: &Path) -> PathBuf {
    sidecar(path, ".tmp")
}

fn sidecar(path: &Path, suffix: &str) -> PathBuf {
    let mut name: OsString = path.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

/// 读主文件；主文件不存在但 `.bak` 在时，从 `.bak` 恢复。
///
/// 主文件存在就**以它为准**，读完顺手清掉可能残留的 `.bak`
/// （那是上一次保存成功但没来得及清理的旧档）。
pub fn read_text(path: &Path) -> io::Result<String> {
    match std::fs::read_to_string(path) {
        Ok(text) => {
            let _ = std::fs::remove_file(backup_path(path));
            Ok(text)
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            let backup = backup_path(path);
            match std::fs::read_to_string(&backup) {
                Ok(text) => {
                    // 放回主文件；失败也不影响这次读取（下次还会走 .bak）。
                    let _ = std::fs::rename(&backup, path);
                    Ok(text)
                }
                Err(backup_error) if backup_error.kind() == io::ErrorKind::NotFound => Err(error),
                Err(backup_error) => Err(backup_error),
            }
        }
        Err(error) => Err(error),
    }
}

/// 原子地把 `text` 写到 `path`（自动创建父目录）。
///
/// 成功返回后，主文件一定是**完整的新内容**；失败时尽量保持旧文件不动。
pub fn write_atomic(path: &Path, text: &str) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let tmp = tmp_path(path);
    let backup = backup_path(path);

    // 先写临时文件：这一步失败时主文件完全没被动过。
    std::fs::write(&tmp, text)?;

    let had_old = path.exists();
    if had_old {
        // Windows 上 `rename` 的目标已存在会失败，所以先清掉上一次的 .bak。
        if backup.exists() {
            std::fs::remove_file(&backup)?;
        }
        // 旧档让位。到这里为止主文件"消失"了一瞬间，但 .bak 是完整的，
        // 下一次 `read_text` 会把它恢复回来。
        std::fs::rename(path, &backup)?;
    }

    match std::fs::rename(&tmp, path) {
        Ok(()) => {
            if had_old {
                // 清理失败不算写失败：主文件已经是新的，残留的 .bak 下次读会清。
                let _ = std::fs::remove_file(&backup);
            }
            Ok(())
        }
        Err(error) => {
            // 新档没换上去：把旧档放回原位，避免读者看到"文件不存在"。
            if had_old {
                let _ = std::fs::rename(&backup, path);
            }
            Err(error)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// 每个测试一个独立目录（名字不同，避免并行测试互相踩）。
    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("wslc-fileio-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("应能建临时目录");
        dir
    }

    #[test]
    fn writes_and_reads_back() {
        let dir = temp_dir("roundtrip");
        let path = dir.join("store.json");

        write_atomic(&path, r#"{"a":1}"#).expect("应能写");
        assert_eq!(read_text(&path).expect("应能读"), r#"{"a":1}"#);
        // 第二次写要覆盖，而不是追加
        write_atomic(&path, r#"{"a":2}"#).expect("应能覆盖");
        assert_eq!(read_text(&path).expect("应能读"), r#"{"a":2}"#);
        // 成功后不应留下 .tmp / .bak
        assert!(!tmp_path(&path).exists(), ".tmp 应该被 rename 走了");
        assert!(!backup_path(&path).exists(), ".bak 应该被清掉");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn creates_parent_directories() {
        let dir = temp_dir("mkdir");
        let path = dir.join("nested").join("store.json");
        write_atomic(&path, "x").expect("应能创建父目录并写");
        assert_eq!(read_text(&path).expect("应能读"), "x");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn recovers_from_backup_when_primary_is_missing() {
        let dir = temp_dir("recover");
        let path = dir.join("store.json");
        let backup = backup_path(&path);

        // 模拟"旧档已经 rename 成 .bak，但新档还没换上去"的崩溃点
        fs::write(&backup, r#"{"old":true}"#).expect("应能写 .bak");
        assert!(!path.exists());

        assert_eq!(
            read_text(&path).expect("应能从 .bak 恢复"),
            r#"{"old":true}"#
        );
        // 恢复后主文件回来了，.bak 被消费掉
        assert_eq!(
            fs::read_to_string(&path).expect("主文件应存在"),
            r#"{"old":true}"#
        );
        assert!(!backup.exists(), "恢复后 .bak 不应留着");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn primary_wins_and_stale_backup_is_removed() {
        let dir = temp_dir("stale");
        let path = dir.join("store.json");
        let backup = backup_path(&path);

        fs::write(&path, "new").expect("应能写主文件");
        fs::write(&backup, "old").expect("应能写 .bak");

        assert_eq!(read_text(&path).expect("主文件应优先"), "new");
        assert!(!backup.exists(), "读到主文件后应清掉旧 .bak");

        let _ = fs::remove_dir_all(&dir);
    }
}
