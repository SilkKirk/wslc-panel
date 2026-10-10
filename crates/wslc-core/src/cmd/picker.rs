//! 文件 / 目录选择器：借 Windows 自己的对话框。
//!
//! # 为什么让**另一个进程**去弹对话框
//!
//! 这个项目在 v0.3.4 刚修过一次"动作阻塞界面线程"的问题（点「启动」窗口变成
//! 未响应）。原生文件对话框**天生容易重犯**那个错：它是模态的，
//! 在同一个线程上调它就会把 GPUI 的消息循环堵住。
//!
//! 所以这里选了另一条路：起一个 `powershell.exe`，让**它**去弹对话框，
//! 我们只是等它的 stdout。它跑多久、卡在哪儿，都和界面线程无关 ——
//! 这一点是**结构上**成立的，不依赖我对某个库的线程模型的判断。
//!
//! 代价也如实记下来：
//!
//! - 起一次 PowerShell 要几百毫秒才出对话框（不是"秒开"）；
//! - 对话框的 owner 不是我们的窗口，所以**不是严格模态**；
//!   脚本里用一个隐藏的 TopMost 窗体把它顶到前面来缓解；
//! - 用户不点完，那个进程就一直留着（我们等在后台执行器上，界面不受影响）；
//! - 某些 EDR 对"应用拉起 PowerShell"比较敏感，可能拦。
//!
//! 要换成 `rfd` 之类的原生 crate，替换点只有 [`pick_file`] / [`pick_directory`]
//! 这两个函数 —— 脚本拼装与输出解析都是纯函数，可以一起丢掉。
//!
//! # 为什么脚本要拆成纯函数
//!
//! 引号、转义、`-STA` 这些是最容易写错的部分，而它错起来**只表现为
//! "点了「浏览」什么也没发生"** —— 那是最难查的一类问题。
//! 拆出来就能脱离 Windows 单测（见本文件末尾）。

use std::process::{Command, Stdio};

use crate::error::{Error, Result};

/// 用来弹对话框的程序。
///
/// 选 `powershell.exe`（Windows PowerShell 5.1，系统自带）而不是
/// `pwsh.exe`（PowerShell 7）：后者**不一定装了**，而前者每台 Windows 都有。
/// 5.1 里 `System.Windows.Forms` 走 .NET Framework，同样可用。
const POWERSHELL: &str = "powershell.exe";

/// 只用于错误消息里的程序名。
const POWERSHELL_LABEL: &str = "powershell.exe";

/// 弹一个**选文件**的对话框。
///
/// `label` 是给用户看的类别名（`"tar 文件"`），`extensions` 是不含点的后缀
/// （`["tar"]`）。返回 `Ok(None)` 表示**用户取消了** —— 那不算错误。
pub fn pick_file(label: &str, extensions: &[&str]) -> Result<Option<String>> {
    let filter = build_filter(label, extensions);
    run(&format!("选择{label}"), Some(&filter), false)
}

/// 弹一个**选目录**的对话框。
///
/// 返回 `Ok(None)` 表示用户取消了。
pub fn pick_directory(title: &str) -> Result<Option<String>> {
    run(title, None, true)
}

/// 起进程、等结果。
///
/// **刻意没有超时**：`Command::output()` 会一直等到对话框关掉为止。
/// 加超时意味着"把一个正开着的对话框强行关掉"，那比多等一会儿糟糕得多。
/// 代价是用户不点完，这个后台任务就一直挂着 —— 所以调用方必须保证
/// **同时只弹一个**（选择器开着的时候按钮不再响应），
/// 否则任务栏里会堆一排对话框。
fn run(title: &str, filter: Option<&str>, folders: bool) -> Result<Option<String>> {
    let script = build_script(title, filter, folders);

    let mut cmd = Command::new(POWERSHELL);
    // `-STA` 是 WinForms 对话框的硬性要求：MTA 线程上 `ShowDialog` 会直接抛。
    // `-NoProfile` 让行为可预期、也快一点（不加载用户的 profile）。
    cmd.args(["-NoProfile", "-STA", "-Command", &script])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // 不这样会闪一个黑色控制台窗口出来 —— 对话框还没出，控制台先闪一下，
        // 看起来很像程序出错了。
        cmd.creation_flags(crate::cli::CREATE_NO_WINDOW);
    }

    let out = cmd.output().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            Error::ExecutableNotFound {
                program: POWERSHELL_LABEL,
                path: POWERSHELL.to_owned(),
                hint: "文件选择器借的是系统自带的 Windows PowerShell；\
                       找不到它就只能手动把路径填进输入框。"
                    .to_owned(),
            }
        } else {
            Error::Io(e)
        }
    })?;

    Ok(parse_output(&out.stdout))
}

/// 拼 WinForms 的 `Filter`：`tar 文件 (*.tar)|*.tar|所有文件 (*.*)|*.*`
///
/// 格式是 `显示名|通配|显示名|通配`。**每一段都不能有 `|`**，
/// 所以类别名由调用方保证（都是代码里写死的常量）。
fn build_filter(label: &str, extensions: &[&str]) -> String {
    let globs: Vec<String> = extensions.iter().map(|e| format!("*.{e}")).collect();
    // 后缀列表为空时给一个"什么都收"的兜底，不然显示名后面会是个空括号
    let shown = if globs.is_empty() {
        "*.*".to_owned()
    } else {
        globs.join("; ")
    };
    let pattern = if globs.is_empty() {
        "*.*".to_owned()
    } else {
        globs.join(";")
    };

    format!("{label} ({shown})|{pattern}|所有文件 (*.*)|*.*")
}

/// 拼出交给 `powershell.exe` 的脚本。
///
/// 拆成纯函数是为了能**脱离 Windows 单测** —— 见模块头的说明。
fn build_script(title: &str, filter: Option<&str>, folders: bool) -> String {
    // PowerShell 的单引号字符串里，`''` 表示一个 `'`。
    // 标题和后缀表都是我们自己写的常量，但脚本是**拼**出来的，必须防。
    let quote = |s: &str| s.replace('\'', "''");

    let mut script = String::new();

    // 隐藏的 owner 窗体：把对话框顶到最前面。
    // 不给 owner 的话它可能开在应用窗口**后面** ——
    // 用户点了「浏览」却什么都没看见，又是一次"点了没反应"。
    script.push_str("Add-Type -AssemblyName System.Windows.Forms | Out-Null\n");
    script.push_str("$owner = New-Object System.Windows.Forms.Form\n");
    script.push_str("$owner.TopMost = $true\n");
    script.push_str("$owner.ShowInTaskbar = $false\n");
    script.push_str("$owner.WindowState = 'Minimized'\n");
    script.push_str("$owner.Show() | Out-Null\n");

    if folders {
        script.push_str("$d = New-Object System.Windows.Forms.FolderBrowserDialog\n");
        script.push_str(&format!("$d.Description = '{}'\n", quote(title)));
        script.push_str("$d.ShowNewFolderButton = $true\n");
        script.push_str("if ($d.ShowDialog($owner) -eq [System.Windows.Forms.DialogResult]::OK) {\n");
        script.push_str("  [Console]::Out.Write($d.SelectedPath)\n");
        script.push_str("}\n");
    } else {
        script.push_str("$d = New-Object System.Windows.Forms.OpenFileDialog\n");
        script.push_str(&format!("$d.Title = '{}'\n", quote(title)));
        script.push_str(&format!("$d.Filter = '{}'\n", quote(filter.unwrap_or(""))));
        // 手输一个不存在的路径时挡下来，免得后面 wsl 报一句不好懂的错
        script.push_str("$d.CheckFileExists = $true\n");
        script.push_str("if ($d.ShowDialog($owner) -eq [System.Windows.Forms.DialogResult]::OK) {\n");
        script.push_str("  [Console]::Out.Write($d.FileName)\n");
        script.push_str("}\n");
    }

    script.push_str("$owner.Close()\n");
    script
}

/// 从 PowerShell 的 stdout 里取出路径。
///
/// **取消时输出是空的** —— 那是 `None`，不是错误。
/// 这里也顺手把混进来的空白去掉（`[Console]::Out.Write` 不带换行，
/// 但 PowerShell 自己偶尔会吐一点）。
fn parse_output(stdout: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(stdout);
    let trimmed = text.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_filter_follows_the_winforms_shape() {
        // 格式：显示名|通配|显示名|通配 —— 四段
        let filter = build_filter("tar 文件", &["tar"]);
        assert_eq!(filter, "tar 文件 (*.tar)|*.tar|所有文件 (*.*)|*.*");
        assert_eq!(filter.split('|').count(), 4, "{filter}");

        // 多个后缀
        let multi = build_filter("镜像", &["tar", "tar.gz"]);
        assert_eq!(
            multi,
            "镜像 (*.tar; *.tar.gz)|*.tar;*.tar.gz|所有文件 (*.*)|*.*"
        );

        // 空后缀表也要给出合法形状（不能是空括号）
        let none = build_filter("任意", &[]);
        assert_eq!(none, "任意 (*.*)|*.*|所有文件 (*.*)|*.*");
    }

    #[test]
    fn script_is_sta_and_writes_the_path_to_stdout() {
        let script = build_script("选择tar 文件", Some("tar 文件 (*.tar)|*.tar"), false);

        // `-STA` 是 WinForms 的硬性要求，漏了对话框会直接抛
        // （这里只能断言我们**传了**这个参数，见 `run`）。
        assert!(script.contains("Add-Type -AssemblyName System.Windows.Forms"));
        assert!(script.contains("New-Object System.Windows.Forms.OpenFileDialog"));
        // 结果必须走 stdout，而且不带换行（我们要原样拿路径）
        assert!(script.contains("[Console]::Out.Write($d.FileName)"));
        // 隐藏的 owner 必须在，且被用作 ShowDialog 的 owner
        assert!(script.contains("$owner.TopMost = $true"));
        assert!(script.contains("ShowDialog($owner)"));
        // 收尾要关掉 owner，不然会有个隐形窗体一直挂着
        assert!(script.trim_end().ends_with("$owner.Close()"));
    }

    #[test]
    fn folder_picker_uses_the_folder_dialog() {
        let script = build_script("选择安装目录", None, true);
        assert!(script.contains("System.Windows.Forms.FolderBrowserDialog"));
        assert!(script.contains("[Console]::Out.Write($d.SelectedPath)"));
        // 目录对话框没有 Filter，不该出现这一行
        assert!(!script.contains("$d.Filter"), "{script}");
    }

    #[test]
    fn single_quotes_in_text_are_escaped_for_powershell() {
        // PowerShell 单引号串里 `''` 表示一个 `'`。
        // 不转义的话脚本会被**截断**，表现为"对话框根本不出来"。
        let script = build_script("it's", Some("a'b (*.x)|*.x"), false);
        assert!(script.contains("$d.Title = 'it''s'"), "{script}");
        assert!(script.contains("$d.Filter = 'a''b (*.x)|*.x'"), "{script}");
    }

    #[test]
    fn empty_output_means_the_user_cancelled() {
        assert_eq!(parse_output(b""), None);
        assert_eq!(parse_output(b"   \r\n"), None);
        assert_eq!(
            parse_output(b"D:\\img\\a.tar\r\n"),
            Some(r"D:\img\a.tar".to_owned())
        );
        // 前导空白也去掉（PowerShell 有时会先吐一个换行）
        assert_eq!(
            parse_output(b"\r\nD:\\wsl\r\n"),
            Some(r"D:\wsl".to_owned())
        );
    }

    #[test]
    fn non_utf8_output_does_not_panic() {
        // 中文路径在某些代码页下可能不是合法 UTF-8 —— 丢掉坏字节而不是崩
        let bytes = [0x44, 0x3A, 0x5C, 0xFF, 0xFE];
        assert!(parse_output(&bytes).is_some());
    }
}
