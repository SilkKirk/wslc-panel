//! 界面用到的纯函数助手。
//!
//! 放在这个 crate 而不是 `app.rs`：它们是纯逻辑，单测不该为了跑它们
//! 去链接 GPUI（见本 crate 的顶层说明）。

use wslc_core::model::ContainerSummary;

/// 秒数说成人话（安装动辄几十分钟，"已用 1830 秒"没人愿意换算）。
pub fn human_secs(secs: u64) -> String {
    if secs < 60 {
        format!("{secs} 秒")
    } else if secs < 3600 {
        format!("{} 分 {:02} 秒", secs / 60, secs % 60)
    } else {
        format!("{} 时 {:02} 分", secs / 3600, (secs % 3600) / 60)
    }
}

/// 容器端口的一行摘要。
///
/// 有宿主端口就写 `宿主:容器`；只声明了容器端口时退回 `容器/协议`。
///
/// 刻意**不显示绑定地址**（`127.0.0.1`）—— 实测默认就是它，
/// 每行都重复一遍只是把列占满。想看完整信息点开详情。
///
/// 没有映射时返回 `—`：空串看起来像"界面坏了"。
pub fn ports_summary(item: &ContainerSummary) -> String {
    let ports = item.item.port_mappings();
    if ports.is_empty() {
        return "—".to_owned();
    }
    ports
        .iter()
        .map(|p| match p.host_port {
            Some(host) => format!("{host}:{}", p.container_port),
            None => format!("{}/{}", p.container_port, p.protocol),
        })
        .collect::<Vec<_>>()
        .join("、")
}

/// 按逗号（中英文）或换行切分，去掉空白项。
///
/// 刻意**不按空格切**：环境变量的值里完全可能有空格
/// （`MESSAGE=hello world`），按空格切会把它切成两条。
pub fn split_list(text: &str) -> Vec<String> {
    text.split([',', '，', '\n', '\r'])
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(str::to_owned)
        .collect()
}

/// 顶部刷新按钮的文案。
///
/// `user_initiated` 为假（3 秒一次的自动刷新）时**永远是「刷新」** ——
/// 自动刷新是后台行为，按钮不该跟着闪。用户点了按钮才是「刷新中…」。
pub fn refresh_label(busy: bool, user_initiated: bool) -> &'static str {
    if busy && user_initiated {
        "刷新中…"
    } else {
        "刷新"
    }
}

/// 去掉文案里的 `**粗体**` 与 `` `代码` `` 标记。
///
/// # 为什么需要它
///
/// 这些文案最终都是**纯文本**渲染的（`div().child(String)`，GPUI 不解析
/// Markdown），标记会原样露给用户。本机实测过：删除发行版的确认框里显示的
/// 就是「将注销发行版 X 并\*\*删除它的根文件系统\*\*」——**恰好出现在最需要
/// 一眼看懂的那一屏**。
///
/// 这个仓库的注释与文案风格就是 Markdown，作者会一直这么写；与其在每条文案里
/// 手工删标记（下一个新文案还会漏），不如在**渲染处**统一清掉。于是它是个
/// "守卫"而不是一次性的修补。
///
/// 只认这两个标记 —— 它们是实际用到的全部语法。刻意**不做**通用 Markdown
/// 解析：那要么引依赖（本仓库不能加，见 `AGENTS.md` §1／§3 的 `--locked`），
/// 要么写一个半成品。
///
/// 单个 `*` 不受影响（命令行里的通配符之类要原样显示）。
pub fn strip_md(text: &str) -> String {
    text.replace("**", "").replace('`', "")
}

#[cfg(test)]
mod tests {
    // 这个 crate 里没有 `gpui_kit` —— 所以也不存在"`use super::*` 会把 gpui
    // 再导出的 `test` 属性宏继承进来、遮蔽内建 `#[test]`"那个坑。
    //
    // 这正是纯逻辑测试应该住在这里的原因：CI 的测试任务会把
    // `wslc-panel` **排除**掉（`cargo test --workspace --exclude wslc-panel`），
    // 写在那个 crate 里的断言根本不会被执行。
    use super::{human_secs, ports_summary, refresh_label, split_list, strip_md};
    use wslc_core::model::{ContainerListItem, ContainerSummary};

    /// 只有 `Ports` 一个字段有意义的样本。
    fn summary_with_ports(ports: &str) -> ContainerSummary {
        ContainerSummary {
            item: ContainerListItem {
                ports: ports.to_owned(),
                ..Default::default()
            },
            stats: None,
        }
    }

    #[test]
    fn human_secs_says_minutes_and_hours() {
        // 不到一分钟就照原样说秒
        assert_eq!(human_secs(0), "0 秒");
        assert_eq!(human_secs(59), "59 秒");
        // 分秒都要补零，免得出现 "3 分 5 秒" 这种对不齐的
        assert_eq!(human_secs(60), "1 分 00 秒");
        assert_eq!(human_secs(125), "2 分 05 秒");
        // 一小时以上只说到分（秒在这个尺度上没有意义）
        assert_eq!(human_secs(3600), "1 时 00 分");
        assert_eq!(human_secs(3900), "1 时 05 分");
        // 安装动辄几十分钟 —— 这是它存在的理由
        assert_eq!(human_secs(1830), "30 分 30 秒");
    }

    #[test]
    fn ports_summary_uses_the_real_fixture() {
        // 没有端口映射 → 破折号（空串看起来像"界面坏了"）
        assert_eq!(ports_summary(&summary_with_ports("")), "—");

        // 真机样本：`container_list.jsonl` 里那个 `127.0.0.1:18080->80/tcp`。
        // 刻意不自己编 Ports 字符串 —— 那样测的是"我以为的解析规则"。
        let items: Vec<ContainerListItem> =
            wslc_core::jsonl::parse_lines(include_str!(
                "../../wslc-core/tests/fixtures/container_list.jsonl"
            ))
            .expect("夹具应该解析得动");
        let real = ContainerSummary {
            item: items.into_iter().next().expect("夹具里有一条"),
            stats: None,
        };
        // 绑定地址（127.0.0.1）刻意不显示
        assert_eq!(ports_summary(&real), "18080:80");
    }

    #[test]
    fn split_list_handles_commas_and_newlines() {
        assert_eq!(split_list("8080:80, 9090:90"), vec!["8080:80", "9090:90"]);
        assert_eq!(split_list("a\nb\r\nc"), vec!["a", "b", "c"]);
        // 中文逗号也认 —— 用户从中文文档里复制粘贴很常见
        assert_eq!(split_list("a，b"), vec!["a", "b"]);
    }

    #[test]
    fn split_list_drops_empty_items() {
        assert!(split_list("").is_empty());
        assert!(split_list("  ,  , \n ").is_empty());
        assert_eq!(split_list(" , a , "), vec!["a"]);
    }

    #[test]
    fn split_list_does_not_split_on_spaces() {
        // 环境变量的值里完全可能有空格，按空格切会把它切成两条
        assert_eq!(
            split_list("MESSAGE=hello world"),
            vec!["MESSAGE=hello world"]
        );
    }

    #[test]
    fn auto_refresh_keeps_button_label_unchanged() {
        // 后台自动刷新期间按钮**不能**变成「刷新中…」——
        // 3 秒一次的话按钮就永远停在「刷新中…」上了。
        assert_eq!(refresh_label(true, false), "刷新");
        // 用户点的按钮才显示进度。
        assert_eq!(refresh_label(true, true), "刷新中…");
        // 空闲时当然是「刷新」。
        assert_eq!(refresh_label(false, false), "刷新");
        assert_eq!(refresh_label(false, true), "刷新");
    }

    #[test]
    fn strip_md_removes_bold_and_code_markers() {
        // 这两条是**本机实测撞到**的原话（确认框正文 / 来源说明）
        assert_eq!(
            strip_md("将删除所有**已停止**的容器及其可写层。"),
            "将删除所有已停止的容器及其可写层。"
        );
        assert_eq!(
            strip_md("本地 ext4 虚拟磁盘（`.vhdx`）会被**拷贝**一份到安装目录。"),
            "本地 ext4 虚拟磁盘（.vhdx）会被拷贝一份到安装目录。"
        );
        // 多个标记混在一起
        assert_eq!(strip_md("**a** 和 `b` 和 **c**"), "a 和 b 和 c");
        assert_eq!(strip_md(""), "");
        // 没有标记的原样返回
        assert_eq!(strip_md("普通文案"), "普通文案");
    }

    #[test]
    fn strip_md_leaves_single_markers_alone() {
        // ⚠️ 只删**成对**的粗体标记，单个 `*` 必须原样留着 ——
        // 命令行里的通配符、`--tail *` 这类东西被吃掉就成了另一条命令。
        assert_eq!(strip_md("wslc ps *"), "wslc ps *");
        assert_eq!(strip_md("*"), "*");
        assert_eq!(strip_md("2 * 3"), "2 * 3");
        // 三个星号里那一对会被删掉（`**` 优先匹配），剩下的单个留着 ——
        // 这是明确取舍：文案里不会出现三个连续星号。
        assert_eq!(strip_md("***"), "*");
    }
}
