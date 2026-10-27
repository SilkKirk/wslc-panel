//! 跨模块的纯函数小工具。
//!
//! 为什么不塞进 `jsonl` / `storage` 这类领域模块：这里放的是"不属于任何领域、
//! 但又不只一处要用"的东西。领域模块名说明不了它，塞进去之后下一个需要它的人
//! 会再写一份。
//!
//! 第一条居民是 [`truncate_chars`]：它原先在 `jsonl.rs`（当时只有 JSON 错误消息
//! 要截断），而面板侧的 `wslc-panel-core` 又**独立写了一份逐字相同**的实现
//! （`util::truncate_chars`），两条注释还各自写了"不能按字节切"。
//! 依赖方向是 `panel-core → core`，所以唯一能两边都用的位置在 core 这一侧。
//!
//! 后来 [`now_secs`] / [`shell_quote`] 也是同一个问题：三处 `now_secs`、
//! 两处 shell 单引号转义，各写各的。安全边界和"当前时间"这种基础件
//! 只该有一份实现。

/// 当前的 Unix 时间（秒）。拿不到系统时间时返回 0。
///
/// 三处调用（更新检查的节流、卡片上的"上次检查"、MySQL 备份文件名）
/// 对 0 都安全：节流最坏多查一次，卡片写"0 秒前"，备份文件名退化成
/// 1970 年 —— 不会 panic，也不会阻塞。
pub fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0)
}

/// 把一个值变成**安全的单引号 shell 字面量**。
///
/// 目标目录、应用名、pidfile 路径、`wsl.conf` 里的用户名都可能来自用户输入，
/// 而它们要拼进 `sh -c` 的脚本文本里。这不是"参数被当成选项"那个问题，
/// 是**真的 shell 注入**：一个叫 `a; rm -rf /` 的目标目录会真的去执行那半截。
///
/// 做法是 POSIX 的经典写法：整体用单引号包起来，内部的 `'` 写成 `'\''`
/// （结束当前单引号串 → 一个转义的 `'` → 重新开始单引号串）。
/// 单引号内部**除 `'` 外没有任何字符有特殊含义**，所以 `$`、反引号、`\`、
/// 换行都不需要额外处理。
pub fn shell_quote(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('\'');
    for ch in value.chars() {
        if ch == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(ch);
        }
    }
    out.push('\'');
    out
}

/// 拼一条**给人看 / 复制**的命令行：含空格或为空的参数加双引号。
///
/// 这不是执行参数转义（执行永远走 `argv`），只是让"等效命令预览"能直接
/// 粘进终端。路径里带空格极常见；空参数不加引号会被 shell 吞掉。
///
/// ⚠️ 只处理"能不能看出参数边界"这一档：参数里若本身含 `"`，这里不负责
/// 转义（显示层面，用户能看出原文即可）。真要在 shell 里精确表达，用
/// [`shell_quote`] 或 Windows 侧自己的规则。
pub fn format_args_for_display(args: &[String]) -> String {
    args.iter()
        .map(|arg| {
            if (arg.is_empty() || arg.contains(' ')) && !arg.starts_with('"') {
                format!("\"{arg}\"")
            } else {
                arg.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// 取字符串前 `max` 个**字节**，落在字符边界上才截，否则原样返回。
///
/// # 这是"id 前 N 位"的唯一实现
///
/// 容器 / 镜像 / 网络各自有一个 `short_id()`，四份都是同一句
/// `self.id.get(..self.id.len().min(12)).unwrap_or(&self.id)` —— 抄了四遍，
/// 连"为什么用 `get` 不用切片"这个坑都只在一处写了注释。
///
/// # 为什么是 `get(..n)` 而不是 `&s[..n]`
///
/// id 来自外部命令的输出，**理论上**是 64 位十六进制，但那不是设计前提：
/// 一个多字节字符正好跨过第 `n` 个字节时，`&s[..n]` 是一次**启动路径上的 panic**。
/// `get` 在非边界上返回 `None`，于是退回整串 —— 显示上"长了一点"，
/// 比崩掉好（`main.rs` 的 `short_build_sha` 是同一个理由）。
///
/// 注意它和 [`truncate_chars`] 不是一件事：那个按**字符**切给人看的文字、
/// 还会补省略号；这个只用于 id 这类**定长标识**，不补任何东西。
pub fn short_prefix(text: &str, max: usize) -> &str {
    text.get(..text.len().min(max)).unwrap_or(text)
}

/// 按**字符**（不是字节）截断，超长时在末尾加一个省略号。
///
/// # 为什么不能按字节切
///
/// `&s[..120]` 落在多字节字符中间会 **panic**，而这里的输入是**外部命令的输出**
/// （`wslc` / `wsl.exe` 什么字符都可能吐）和用户填的名字 —— 也就是说这个函数
/// 专门处理"我们完全控制不了"的字符串，碰上一个中日韩字符就崩。
///
/// 超长时截到 `max` 个字符再补 `…`，所以返回值的字符数是 `max + 1`；
/// 正好等于 `max` 时**不**截（没有省略号）。
pub fn truncate_chars(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_owned();
    }
    let mut out: String = text.chars().take(max).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_quote_handles_the_only_character_that_matters() {
        assert_eq!(shell_quote("abc"), "'abc'");
        assert_eq!(shell_quote(""), "''");
        assert_eq!(shell_quote("a b"), "'a b'");
        assert_eq!(shell_quote("it's"), r"'it'\''s'");
        assert_eq!(shell_quote("'"), r"''\'''");
    }

    #[test]
    fn shell_quote_neutralizes_the_usual_injection_payloads() {
        // 不要求这些字符串"看起来对"，只要求它们不会从单引号串里逃出来：
        // 每个 `'` 都必须被拼成 `'\''`。
        for raw in ["; rm -rf /", "$(whoami)", "`id`", "a\nb"] {
            let quoted = shell_quote(raw);
            assert!(quoted.starts_with('\''), "{quoted}");
            assert!(quoted.ends_with('\''), "{quoted}");
            assert_eq!(
                quoted.matches(r"'\''").count(),
                raw.matches('\'').count(),
                "转义数量不对：{raw:?} → {quoted}"
            );
        }
    }

    #[test]
    fn display_args_quote_spaces_and_empty_values() {
        let args = vec!["run".to_owned(), "a b".to_owned(), String::new()];
        assert_eq!(format_args_for_display(&args), "run \"a b\" \"\"");
        // 已经带引号的参数不重复套（用户手写的预览可能已经加过）
        let quoted = vec!["\"a b\"".to_owned()];
        assert_eq!(format_args_for_display(&quoted), "\"a b\"");
        assert_eq!(format_args_for_display(&[]), "");
    }

    #[test]
    fn now_secs_is_not_zero_on_a_sane_clock() {
        // 不比较具体时间（会漂），只确认它没落在"拿不到时间"的兜底上。
        if std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .is_ok()
        {
            assert!(now_secs() > 0);
        }
    }

    #[test]
    fn truncate_is_char_safe() {
        // 300 个 3 字节字符：按字节切第一个就越界，按字符切只会得到 200 + 省略号
        let s = "中".repeat(300);
        let t = truncate_chars(&s, 200);
        assert_eq!(t.chars().count(), 201);
        assert!(t.ends_with('…'));
    }

    #[test]
    fn text_within_the_limit_is_returned_verbatim() {
        // 正好等于上限时**不**加省略号 —— 否则一个刚好 200 字的摘要会莫名多出一个 …
        assert_eq!(truncate_chars("abc", 3), "abc");
        assert_eq!(truncate_chars("", 0), "");
        // 差一个字符才真的截
        assert_eq!(truncate_chars("abcd", 3), "abc…");
    }

    // ---- `short_prefix`（容器 / 镜像 / 网络 / 构建号的短 ID）----

    #[test]
    fn short_prefix_takes_the_first_max_bytes() {
        // 真机形状：64 位十六进制 ID
        assert_eq!(
            short_prefix("abcdef0123456789abcdef0123456789", 12),
            "abcdef012345"
        );
        // 构建号用 7 位
        assert_eq!(short_prefix("cbee728ea3b1f42aa", 7), "cbee728");
    }

    #[test]
    fn short_prefix_keeps_short_input_whole() {
        // 比上限还短：原样返回，不补空格、不报错
        assert_eq!(short_prefix("dev", 7), "dev");
        assert_eq!(short_prefix("", 12), "");
        // 正好等于上限
        assert_eq!(short_prefix("abcdefghijkl", 12), "abcdefghijkl");
    }

    #[test]
    fn short_prefix_never_splits_a_multibyte_char() {
        // ⚠️ 这条就是"用 `get` 不用切片"的理由本身。
        // `"abcdefghijk中"`：ASCII 11 个字节，`中` 占 3 字节 → 第 12 个字节
        // 落在 `中` 的中间。`&s[..12]` 在这里**直接 panic**；
        // `short_prefix` 退回整串（显示上长一个字符，总比崩了强）。
        let s = "abcdefghijk中";
        assert_eq!(short_prefix(s, 12), s, "非字符边界必须退回整串");
        // 边界正好落在一个字符结束处时才真的截
        assert_eq!(short_prefix(s, 11), "abcdefghijk");
    }
}
