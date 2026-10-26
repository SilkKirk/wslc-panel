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
}
