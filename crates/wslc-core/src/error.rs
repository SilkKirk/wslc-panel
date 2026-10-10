//! 错误类型。
//!
//! # 为什么进程类错误都带 `program`
//!
//! 本 crate 现在要驱动**两个**外部程序：
//!
//! - `wslc.exe` —— WSL 容器（[`crate::cli::Wslc`]）
//! - `wsl.exe` —— WSL 发行版（[`crate::cli::Wsl`]）
//!
//! 早先的版本把 `"wslc"` 写死在错误消息里（`"找不到 wslc 可执行文件"`）。
//! 接上 `wsl.exe` 之后，用户明明是在管发行版，却看到一句
//! 「找不到 wslc 可执行文件」，会被直接带偏。
//!
//! 所以进程相关的变体都让调用方把**程序名**和**对应的环境变量名**传进来。
//! 不涉及具体程序的变体（[`Error::Parse`] / [`Error::Settings`] /
//! [`Error::InvalidArgument`]）保持原样。

use std::time::Duration;

/// `wslc-core` 的统一错误类型。
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// 找不到可执行文件。
    ///
    /// `hint` 是**给用户的下一步**，由调用方决定 —— 因为不同程序找不回来的
    /// 原因和补救方式完全不同（`wslc`/`wsl` 要装 WSL 或设 `WSLC_PATH`/`WSL_PATH`；
    /// `reg.exe` 随 Windows 提供，只可能是 `PATH` 出了问题）。
    #[error("找不到 {program} 可执行文件：{path}。{hint}")]
    ExecutableNotFound {
        /// 程序名，只用于消息（`"wslc"` / `"wsl"` / `"reg"`）。
        program: &'static str,
        /// 期望的路径（或交给 `PATH` 解析的文件名）。
        path: String,
        /// 怎么把它找回来。
        hint: String,
    },

    /// 子进程 IO 错误。
    ///
    /// 这个变体**不**带 `program`：它是 `#[from]` 自动转换的目标，
    /// 加了字段就没法用 `?` 了。消息里也不点名具体程序。
    #[error("子进程执行失败：{0}")]
    Io(#[from] std::io::Error),

    /// 命令超时。
    #[error("{program} {args} 执行超时（超过 {timeout:?}）")]
    Timeout {
        /// 程序名（`"wslc"` / `"wsl"`）。
        program: &'static str,
        /// 被执行的参数（用于报错展示）。
        args: String,
        /// 配置的超时时间。
        timeout: Duration,
    },

    /// 命令被调用方主动取消。
    #[error("{program} {args} 已被取消")]
    Cancelled {
        /// 程序名（`"wslc"` / `"wsl"`）。
        program: &'static str,
        /// 被执行的参数。
        args: String,
    },

    /// 命令返回非零退出码。
    #[error("{program} {args} 返回退出码 {code}：{stderr}")]
    NonZeroExit {
        /// 程序名（`"wslc"` / `"wsl"`）。
        program: &'static str,
        /// 被执行的参数。
        args: String,
        /// 退出码。
        ///
        /// ⚠️ **不要假设它是 1**。实测 `wsl.exe -d <不存在的发行版>` 返回 **-1**。
        code: i32,
        /// 标准错误输出（已合并标准输出，便于定位）。
        stderr: String,
    },

    /// 输出解析失败。
    #[error("解析输出失败：{0}")]
    Parse(String),

    /// 配置文件读写失败。
    #[error("配置文件操作失败：{0}")]
    Settings(String),

    /// 调用方传入了非法参数（不会真的执行命令）。
    #[error("参数错误：{0}")]
    InvalidArgument(String),

    /// 多步流程（安装 / 重定位）中途失败。
    ///
    /// 和 [`Error::NonZeroExit`] 的区别：那个是"某条命令失败了"，
    /// 这个的原因可能根本不是命令（建不出目录、等注册超时、内部参数缺失）。
    /// 消息本身就是**给用户的一句话**，所以这里不再套前缀。
    #[error("{0}")]
    Install(String),
}

impl Error {
    /// 返回适合直接展示给用户的简短描述。
    ///
    /// 目前与 `Display` 一致，保留此方法是为了后续做 i18n 或错误分类。
    pub fn user_message(&self) -> String {
        self.to_string()
    }
}

/// crate 级 `Result`。
pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn executable_not_found_names_the_right_program_and_hint() {
        let wsl = Error::ExecutableNotFound {
            program: "wsl",
            path: "wsl".to_owned(),
            hint: "请设置环境变量 WSL_PATH 指向它".to_owned(),
        };
        let text = wsl.to_string();
        assert!(text.contains("wsl 可执行文件"), "{text}");
        assert!(text.contains("WSL_PATH"), "{text}");
        // 关键：不能再出现另一个程序的名字，否则用户会被带偏
        assert!(!text.contains("wslc"), "{text}");
    }

    #[test]
    fn process_errors_carry_the_program_label() {
        let timeout = Error::Timeout {
            program: "wsl",
            args: "--list --online".to_owned(),
            timeout: Duration::from_secs(30),
        };
        assert!(
            timeout.to_string().starts_with("wsl --list --online"),
            "{timeout}"
        );

        let cancelled = Error::Cancelled {
            program: "wslc",
            args: "pull alpine".to_owned(),
        };
        assert!(
            cancelled.to_string().starts_with("wslc pull alpine"),
            "{cancelled}"
        );

        let exit = Error::NonZeroExit {
            program: "wsl",
            args: "--status".to_owned(),
            code: -1,
            stderr: "不存在具有所提供名称的分发。".to_owned(),
        };
        let text = exit.to_string();
        assert!(text.contains("退出码 -1"), "{text}");
        assert!(text.contains("不存在具有所提供名称的分发"), "{text}");
    }

    #[test]
    fn program_agnostic_variants_do_not_name_a_program() {
        // 这几个变体不该被硬编码进任何程序名 —— 两个域都会用
        for text in [
            Error::Parse("坏行".to_owned()).to_string(),
            Error::Settings("读不到".to_owned()).to_string(),
            Error::InvalidArgument("卷名不能为空".to_owned()).to_string(),
        ] {
            assert!(!text.contains("wslc"), "{text}");
        }
    }

    #[test]
    fn user_message_matches_display() {
        let err = Error::InvalidArgument("x".to_owned());
        assert_eq!(err.user_message(), err.to_string());
    }
}
