//! 面板侧的更新状态：**什么时候查、查到了要不要提示、卡片上写什么**。
//!
//! 不碰网络（那是 [`wslc_core::cmd::update::check`]），也不碰 GPUI
//! （`views.rs` 只读这里的 [`Phase`]）。放这个 crate 是为了让「节流判据」和
//! 「提示判据」能被 `core` job 的单测覆盖 —— 这两条错了的表现是
//! "每天弹一次"或"永远不弹"，都是靠肉眼很难发现的那类问题。

use wslc_core::update::{Release, Version, is_newer};

use crate::prefs::Prefs;

/// 自动检查的最小间隔：24 小时。
///
/// 为什么不是更短：GitHub 接口未认证的配额是 **60 次/小时/IP**，而且
/// **304 也算一次**（实测 remaining 从 52 掉到 51）。一天一次既够用，
/// 也不会因为共享出口 IP 被别人用光而误伤自己。
pub const CHECK_INTERVAL_SECS: u64 = 24 * 60 * 60;

/// 「关于」页那张更新卡的状态。
///
/// 全是纯数据：`views.rs` 只读它，`app.rs` 负责迁移。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Phase {
    /// 还没检查过（或者这次会话关掉了自动检查）。
    #[default]
    Idle,
    /// 正在查。
    Checking,
    /// 查过了，就是最新。
    UpToDate {
        /// 查到的时间（Unix 秒），用来在界面上写"上次检查：N 小时前"。
        checked_at: u64,
    },
    /// 有新版本。
    Available {
        /// GitHub 给的那份信息。
        release: Release,
        /// 用户点过「跳过这个版本」。
        ///
        /// 有这个字段（而不是直接从 `Phase` 里删掉）是因为卡片上要**显示**
        /// "vX.Y.Z 已被跳过" —— 让人看得见自己做过什么决定。
        skipped: bool,
    },
    /// 查失败了。**只有手动检查才会进这个状态**（见 [`crate::update::should_auto_check`]
    /// 的调用点：自动检查失败只写日志）。
    Failed {
        /// 能直接给用户看的一句话。
        message: String,
    },
}

/// 现在该不该自动检查？
///
/// 四条**全部**满足才查：
///
/// 1. 偏好里开着（默认开）；
/// 2. 不是开发构建 —— `WSLC_PANEL_BUILD_SHA` 在本地构建时是 `"dev"`，
///    那种构建的版本号和发布版一样，提示更新纯属干扰；
/// 3. 离上次检查超过 [`CHECK_INTERVAL_SECS`]；
/// 4. 时钟没倒退 —— 手工把系统时间往回调、或者上次记录的时间在未来时，
///    `saturating_sub` 得 0，于是"不到间隔"，不会因此狂发请求。
///
/// `now_secs` 由调用方传（`SystemTime::now()`），这样这个函数能单测。
pub fn should_auto_check(prefs: &Prefs, now_secs: u64, dev_build: bool) -> bool {
    if !prefs.auto_check_update {
        return false;
    }
    if dev_build {
        return false;
    }

    match prefs.last_update_check {
        Some(last) => now_secs.saturating_sub(last) >= CHECK_INTERVAL_SECS,
        None => true,
    }
}

/// 用户是不是点过「跳过这个版本」？
///
/// 存的是用户当时看到的那串版本号，而 `0.10.0` / `v0.10.0` 两种写法都可能
/// 被存下来（界面上显示的是前者，日志里常见后者），所以**两种都比**。
/// 大小写也不该影响判断。
pub fn is_skipped(release: &Release, skipped: Option<&str>) -> bool {
    let Some(skipped) = skipped else {
        return false;
    };
    let skipped = skipped.trim();
    if skipped.is_empty() {
        return false;
    }
    skipped.eq_ignore_ascii_case(&release.raw_version) || skipped.eq_ignore_ascii_case(&release.tag)
}

/// 这次的结果要不要提示？
///
/// 除了"版本确实更新"，还要排掉两件事：
///
/// - 用户点过「跳过这个版本」（见 [`is_skipped`]）；
/// - 当前跑的是比远端**更新**的版本（`is_newer` 自然为假）。
pub fn should_notify(current: &Version, release: &Release, skipped: Option<&str>) -> bool {
    if is_skipped(release, skipped) {
        return false;
    }
    is_newer(&release.version, current)
}

/// 卡片上那行状态文案。
///
/// `now_secs` 是为了算"上次检查：N 前"。纯函数，好测；文案集中在这里
/// 而不是散在 `views.rs` 里，是为了让"四种状态各写一句话"这件事只发生一次。
pub fn status_line(phase: &Phase, now_secs: u64) -> String {
    match phase {
        Phase::Idle => "还没检查过更新".to_owned(),
        Phase::Checking => "正在检查更新…".to_owned(),
        Phase::UpToDate { checked_at } => {
            let age = now_secs.saturating_sub(*checked_at);
            format!("已是最新（上次检查：{}前）", crate::util::human_secs(age))
        }
        Phase::Available { release, skipped } => {
            if *skipped {
                return format!("{} 已跳过，不再提示", release.raw_version);
            }
            match release.published.as_deref() {
                Some(date) => format!("有新版本 {}（{date} 发布）", release.raw_version),
                None => format!("有新版本 {}", release.raw_version),
            }
        }
        Phase::Failed { message } => message.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(text: &str) -> Version {
        Version::parse(text).expect("测试里的版本号都该能解析")
    }

    fn release(raw: &str) -> Release {
        Release {
            version: v(raw),
            raw_version: raw.to_owned(),
            tag: format!("v{raw}"),
            published: Some("2026-10-19".to_owned()),
            page_url: "https://example.com/releases".to_owned(),
            notes_head: "变化".to_owned(),
        }
    }

    // -- 节流 -------------------------------------------------------------

    #[test]
    fn auto_check_when_nothing_recorded() {
        let prefs = Prefs::default();
        assert!(prefs.auto_check_update, "默认应该开着");
        assert!(should_auto_check(&prefs, 1_000, false));
    }

    #[test]
    fn respects_the_switch() {
        let prefs = Prefs {
            auto_check_update: false,
            ..Prefs::default()
        };
        assert!(!should_auto_check(&prefs, 1_000_000, false));
    }

    #[test]
    fn skips_dev_builds() {
        // 本地构建的版本号和发布版一样，提示更新只会干扰开发。
        let prefs = Prefs::default();
        assert!(!should_auto_check(&prefs, 1_000_000, true));
    }

    #[test]
    fn throttles_within_24_hours_and_allows_after() {
        let base = 1_000_000;
        let prefs = Prefs {
            last_update_check: Some(base),
            ..Prefs::default()
        };

        assert!(
            !should_auto_check(&prefs, base + CHECK_INTERVAL_SECS - 1, false),
            "不到 24 小时不查"
        );
        assert!(
            should_auto_check(&prefs, base + CHECK_INTERVAL_SECS, false),
            "正好 24 小时可以查"
        );
        assert!(should_auto_check(&prefs, base + 86400 * 3, false));
    }

    #[test]
    fn clock_going_backwards_does_not_spam() {
        let prefs = Prefs {
            last_update_check: Some(2_000_000),
            ..Prefs::default()
        };
        // 系统时间被调回过去：不能因为 saturating_sub 得 0 就判成"该查"。
        // （判成 false 的代价是"这次不查"，判成 true 的代价是"每次都查"。）
        assert!(!should_auto_check(&prefs, 1_000_000, false));
    }

    // -- 提示 -------------------------------------------------------------

    #[test]
    fn notifies_only_for_newer_versions() {
        let current = v("0.9.25");
        assert!(should_notify(&current, &release("0.10.0"), None));
        assert!(!should_notify(&current, &release("0.9.25"), None));
        assert!(!should_notify(&current, &release("0.9.24"), None));
    }

    #[test]
    fn skip_accepts_both_tag_forms() {
        let current = v("0.9.25");
        let release = release("0.10.0");

        // 用户当时看到的是 `0.10.0`，存下来的就是它
        assert!(!should_notify(&current, &release, Some("0.10.0")));
        // 也有人会存 tag 形式
        assert!(!should_notify(&current, &release, Some("v0.10.0")));
        // 大小写不该影响判断
        assert!(!should_notify(&current, &release, Some("V0.10.0")));

        // 跳过一个版本不该顺手把别的版本也跳过
        assert!(should_notify(&current, &release, Some("0.9.99")));
        // 空白等于没填
        assert!(should_notify(&current, &release, Some("   ")));
    }

    // -- 文案 -------------------------------------------------------------

    #[test]
    fn every_phase_has_a_line() {
        let now = 1_000_000;
        for phase in [
            Phase::Idle,
            Phase::Checking,
            Phase::UpToDate { checked_at: now },
            Phase::Available {
                release: release("0.10.0"),
                skipped: false,
            },
            Phase::Available {
                release: release("0.10.0"),
                skipped: true,
            },
            Phase::Failed {
                message: "连不上 api.github.com".to_owned(),
            },
        ] {
            let line = status_line(&phase, now);
            assert!(!line.trim().is_empty(), "{phase:?} 不该是空文案");
        }
    }

    #[test]
    fn available_line_carries_version_and_date() {
        let phase = Phase::Available {
            release: release("0.10.0"),
            skipped: false,
        };
        let line = status_line(&phase, 0);
        assert!(line.contains("0.10.0"), "{line}");
        assert!(line.contains("2026-10-19"), "{line}");
    }

    #[test]
    fn skipped_line_says_so() {
        let phase = Phase::Available {
            release: release("0.10.0"),
            skipped: true,
        };
        let line = status_line(&phase, 0);
        assert!(line.contains("跳过"), "{line}");
    }

    #[test]
    fn uptodate_line_shows_the_age() {
        let now = 1_000_000;
        let phase = Phase::UpToDate {
            checked_at: now - 3 * 3600,
        };
        let line = status_line(&phase, now);
        assert!(line.contains("已是最新"), "{line}");
        assert!(line.contains("3 时"), "要写出多久之前：{line}");
    }

    #[test]
    fn uptodate_handles_a_future_timestamp() {
        // 时钟倒退时不 panic、也不显示负数。
        let phase = Phase::UpToDate {
            checked_at: 2_000_000,
        };
        let line = status_line(&phase, 1_000_000);
        assert!(line.contains("已是最新"), "{line}");
    }

    #[test]
    fn default_phase_is_idle() {
        assert_eq!(Phase::default(), Phase::Idle);
    }
}
