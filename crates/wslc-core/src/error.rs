//! 错误类型。

use std::time::Duration;

/// `wslc-core` 的统一错误类型。
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// 找不到 `wslc` 可执行文件。
    #[error("找不到 wslc 可执行文件：{0}。请安装 WSL 3.0 以上版本，或设置环境变量 WSLC_PATH 指向 wslc.exe")]
    ExecutableNotFound(String),

    /// 子进程 IO 错误。
    #[error("执行 wslc 失败：{0}")]
    Io(#[from] std::io::Error),

    /// 命令超时。
    #[error("wslc {args} 执行超时（超过 {timeout:?}）")]
    Timeout {
        /// 被执行的参数（用于报错展示）。
        args: String,
        /// 配置的超时时间。
        timeout: Duration,
    },

    /// 命令被调用方主动取消。
    #[error("wslc {args} 已被取消")]
    Cancelled {
        /// 被执行的参数。
        args: String,
    },

    /// 命令返回非零退出码。
    #[error("wslc {args} 返回退出码 {code}：{stderr}")]
    NonZeroExit {
        /// 被执行的参数。
        args: String,
        /// 退出码。
        code: i32,
        /// 标准错误输出（已合并标准输出，便于定位）。
        stderr: String,
    },

    /// 输出解析失败。
    #[error("解析 wslc 输出失败：{0}")]
    Parse(String),

    /// 配置文件读写失败。
    #[error("配置文件操作失败：{0}")]
    Settings(String),

    /// 调用方传入了非法参数（不会真的执行命令）。
    #[error("参数错误：{0}")]
    InvalidArgument(String),
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
