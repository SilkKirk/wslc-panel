//! 界面用到的纯函数助手。
//!
//! 放在这个 crate 而不是 `app/`：它们是纯逻辑，单测不该为了跑它们
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

/// 当前的 Unix 时间（秒）。
///
/// 拿不到就返回 0：调用点只有两处（更新检查的节流、卡片上的"上次检查"），
/// 两处对 0 都安全 —— 节流判据会因此认为"该检查了"（最坏多查一次，
/// 而 60 次/小时的配额够用）；卡片上写"0 秒前"，属于可接受的退让。
///
/// 放在这里而不是各调用点自己写：三个地方各写一遍 `SystemTime::now()` +
/// `duration_since` + `unwrap_or_default` 只会多出三种细微不同的写法。
pub fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0)
}

/// 页头那句「最后刷新：N 秒前」。
///
/// 0 / 1 秒都写「刚刚」：自动刷新默认 3 秒，刚刷完就显示「1 秒前」
/// 会让人以为数据是旧的。其余复用 [`human_secs`] 再加「前」。
pub fn refresh_age_label(secs: u64) -> String {
    if secs < 2 {
        "刚刚".to_owned()
    } else {
        format!("{}前", human_secs(secs))
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
/// 刻意**不按空格切**：卷路径、端口映射里完全可能有空格
/// （`E:\my code:/app`），按空格切会把它切成两条。
pub fn split_list(text: &str) -> Vec<String> {
    text.split([',', '，', '\n', '\r'])
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(str::to_owned)
        .collect()
}

/// 环境变量专用切分：换行一定分隔，逗号**只在后面看起来是新的 `KEY=` 时**才分隔。
///
/// # 为什么不能直接用 [`split_list`]（v0.9.20 修）
///
/// 环境变量的值里逗号非常常见：
///
/// ```text
/// SPRING_PROFILES_ACTIVE=dev,local
/// CORS_ALLOWED_ORIGINS=https://a.example,https://b.example
/// JAVA_TOOL_OPTIONS=-Dfoo=a,b
/// ```
///
/// 按逗号一刀切，后半段没有 `=` 会被 `to_spec` 静默丢掉 —— 界面上不报错，
/// 但 Java 进程起来后行为和预期不一样，属于最难查的一类问题。
///
/// 同时也不能只认换行：老用户可能一直按界面提示写 `A=1,B=2`。
/// 所以这里保留逗号分隔能力，但用「逗号后面是不是一个合法的 `KEY=`」来判断：
///
/// - `A=1,B=2` → `["A=1", "B=2"]`
/// - `A=1, B=2` → 同上（逗号后的空格会先 trim）
/// - `SPRING_PROFILES_ACTIVE=dev,local` → 一条
/// - `CORS=https://a,https://b` → 一条
/// - `A=x,B=y,z` → `["A=x", "B=y,z"]`
///
/// ⚠️ 这是启发式，不是完整的 shell 解析：`A=x,y=B` 这种"值里刚好出现
/// `,y=`"的形状会被切错。环境变量名必须是 `[A-Za-z_][A-Za-z0-9_]*`，
/// 现实里这种值极少；真需要精确控制就一行一个，换行永远不会误切。
pub fn split_env_list(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in text.split(['\n', '\r']) {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let mut start = 0usize;
        for (index, ch) in line.char_indices() {
            if ch != ',' && ch != '，' {
                continue;
            }
            let after = index + ch.len_utf8();
            if looks_like_env_assignment(line[after..].trim_start()) {
                push_env_item(&line[start..index], &mut out);
                start = after;
            }
        }
        push_env_item(&line[start..], &mut out);
    }
    out
}

/// 一段是不是以合法的 `KEY=` 开头（用来判断逗号是不是分隔符）。
fn looks_like_env_assignment(text: &str) -> bool {
    let Some((key, _)) = text.split_once('=') else {
        return false;
    };
    let mut chars = key.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !(first.is_ascii_alphabetic() || first == '_') {
        return false;
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// 收集一段环境变量；顺手清掉连续逗号留下的空段 / 尾逗号。
fn push_env_item(segment: &str, out: &mut Vec<String>) {
    // `trim_matches` 而不是 `trim_end_matches`：`"  ,  , "` 这种"只有分隔符"
    // 的段两边都要清，否则会留下一个 `","` 被当成环境变量。
    let segment = segment
        .trim()
        .trim_matches(|c| c == ',' || c == '，')
        .trim();
    if !segment.is_empty() {
        out.push(segment.to_owned());
    }
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

/// 用量的档位（决定进度条与数字用哪个颜色）。
///
/// 放在这里而不是 `views/`：它只是一串**阈值**，而阈值恰恰是"改着改着就错了、
/// 还没人发现"的那类东西（把 85 顺手调成 80，界面照跑、没人看得出来）。
/// `views/` 里的断言 CI 根本不会跑（`cargo test --workspace --exclude wslc-panel`），
/// 所以判定归这里。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoadLevel {
    /// 轻载 —— 界面上用主色。
    Low,
    /// 中载（≥ 60%）—— 界面上用警告色。
    Medium,
    /// 重载（≥ 85%）—— 界面上用危险色。
    High,
}

/// 把占用百分比分档。
///
/// **拿不到值时传 `0.0`**，别传一个负数去"表达没有数据"：`wslc stats` 对已经停掉的
/// 容器给的本来就是 0，而"没有数据"不该画成"要出事了"。
///
/// ⚠️ `NaN` 落进 [`LoadLevel::Low`]，这是**刻意的**：`parse_percent` 对
/// `nan%` 会返回 `NaN`（`"nan".parse::<f64>()` 是 `Ok(NaN)`），而两个比较
/// （`NaN >= 85.0`、`NaN >= 60.0`）**都是 false** —— 于是界面画出来是一条空条，
/// 而不是满格爆红。换成"先判 `< 60` 再判 `< 85`"的写法就会把这个特性弄丢。
pub fn load_level(percent: f64) -> LoadLevel {
    if percent >= 85.0 {
        LoadLevel::High
    } else if percent >= 60.0 {
        LoadLevel::Medium
    } else {
        LoadLevel::Low
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

/// 字节 → 人类可读（1024 进制）。
///
/// 放在这里而不是 `state`：它是个**纯格式化**函数，和 `human_secs` 同一类，
/// 既不属于任何一个状态域，又被状态层（确认弹窗的文案"将删除约 18.32 GB"）
/// 和渲染层同时用到。
pub fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0usize;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.2} {}", UNITS[unit])
    }
}

/// 按**字符**（不是字节）截断，超长时加省略号。
///
/// ⚠️ 实现只有一份：`wslc_core::util::truncate_chars` —— 这里只是转发。
///
/// 为什么不在这个 crate 里自己实现：`wslc-core` 的 `jsonl` 要截断**同一种东西**
/// （外部命令的输出，什么字符都可能有），而依赖方向是 `panel-core → core`，
/// 所以只有 core 那份能两边都用。原先两处是**各自独立写出来**的，
/// 连"不能按字节切"的注释都各写了一遍 —— 那正是该合并的信号
/// （`docs/AUDIT-2026-10-26.md` §2.5）。
///
/// 不能按字节切：`&s[..120]` 落在多字节字符中间会 panic。
pub fn truncate_chars(text: &str, max: usize) -> String {
    wslc_core::util::truncate_chars(text, max)
}

#[cfg(test)]
mod tests {
    // 这个 crate 里没有 `gpui_kit` —— 所以也不存在"`use super::*` 会把 gpui
    // 再导出的 `test` 属性宏继承进来、遮蔽内建 `#[test]`"那个坑。
    //
    // 这正是纯逻辑测试应该住在这里的原因：CI 的测试任务会把
    // `wslc-panel` **排除**掉（`cargo test --workspace --exclude wslc-panel`），
    // 写在那个 crate 里的断言根本不会被执行。
    use super::{
        LoadLevel, human_secs, load_level, ports_summary, refresh_label, split_env_list,
        split_list, strip_md,
    };
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
        let items: Vec<ContainerListItem> = wslc_core::jsonl::parse_lines(include_str!(
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
        // 卷路径 / 端口里也可能有空格，按空格切会把它切成两条
        assert_eq!(split_list(r"E:\my code:/app"), vec![r"E:\my code:/app"]);
    }

    #[test]
    fn split_env_list_keeps_commas_inside_values() {
        // 这三条是实际会写进 Java / Spring 配置的形状 —— 逗号属于值，
        // 按 `split_list` 切会把后半段变成没有 `=` 的段，然后被静默丢掉。
        assert_eq!(
            split_env_list("SPRING_PROFILES_ACTIVE=dev,local"),
            vec!["SPRING_PROFILES_ACTIVE=dev,local"]
        );
        assert_eq!(
            split_env_list("CORS_ALLOWED_ORIGINS=https://a.example,https://b.example"),
            vec!["CORS_ALLOWED_ORIGINS=https://a.example,https://b.example"]
        );
        assert_eq!(
            split_env_list("JAVA_TOOL_OPTIONS=-Dfoo=a,b"),
            vec!["JAVA_TOOL_OPTIONS=-Dfoo=a,b"]
        );
    }

    #[test]
    fn split_env_list_still_accepts_comma_separated_pairs() {
        // 老习惯（界面以前写着"逗号或换行"）不能一夜之间失效：
        // 逗号后面只要是一个合法的 `KEY=`，仍按分隔符处理。
        assert_eq!(split_env_list("A=1,B=2"), vec!["A=1", "B=2"]);
        assert_eq!(split_env_list("A=1, B=2"), vec!["A=1", "B=2"]);
        assert_eq!(split_env_list("A=1，B=2"), vec!["A=1", "B=2"]);
        assert_eq!(split_env_list("A=1\nB=2"), vec!["A=1", "B=2"]);
    }

    #[test]
    fn split_env_list_handles_empty_and_mixed_segments() {
        assert!(split_env_list("").is_empty());
        assert!(split_env_list("  ,  , \n ").is_empty());
        // 连续逗号不产生空条目；后面的 `B=2` 仍要保住
        assert_eq!(split_env_list("A=1,,B=2"), vec!["A=1", "B=2"]);
        // 逗号后是值的一部分（没有 `KEY=`），整段保留
        assert_eq!(split_env_list("A=x,B=y,z"), vec!["A=x", "B=y,z"]);
        // CRLF 也要吃掉
        assert_eq!(split_env_list("A=1\r\nB=2"), vec!["A=1", "B=2"]);
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

    #[test]
    fn load_level_breaks_at_sixty_and_eighty_five() {
        assert_eq!(load_level(0.0), LoadLevel::Low);
        // 边界两侧都要钉住：改阈值时这个测试就是那条"你会被拦下来"的线
        assert_eq!(load_level(59.9), LoadLevel::Low);
        assert_eq!(load_level(60.0), LoadLevel::Medium);
        assert_eq!(load_level(84.9), LoadLevel::Medium);
        assert_eq!(load_level(85.0), LoadLevel::High);
        assert_eq!(load_level(100.0), LoadLevel::High);
        // 统计口径异常（CPU 超过 100% 在多核上是正常的）不该 panic，
        // 也不该"溢出"成别的档位。
        assert_eq!(load_level(320.0), LoadLevel::High);
    }

    #[test]
    fn load_level_treats_missing_and_nan_data_as_low() {
        // "没有数据"用 0.0 表达（见函数说明）—— 是轻载，不是重载。
        assert_eq!(load_level(0.0), LoadLevel::Low);
        // ⚠️ 这一条是"别把 `>= 85` 改成 `< 60 / < 85 的嵌套"的理由：
        // `parse_percent` 对 `nan%` 会给回 NaN，而 NaN 与任何数比较都是 false。
        // 若改成"先判小"，NaN 会掉进最后那个 else（= 重载），界面上就是
        // 一条**满格的红条** —— 而真实情况是"我们没读到数"。
        assert_eq!(load_level(f64::NAN), LoadLevel::Low);
        // 负数（理论上不会出现）同样按轻载处理，不进警告区
        assert_eq!(load_level(-1.0), LoadLevel::Low);
    }

    #[test]
    fn refresh_age_label_says_just_now_for_the_first_two_seconds() {
        assert_eq!(super::refresh_age_label(0), "刚刚");
        assert_eq!(super::refresh_age_label(1), "刚刚");
        assert_eq!(super::refresh_age_label(2), "2 秒前");
        assert_eq!(super::refresh_age_label(59), "59 秒前");
        assert_eq!(super::refresh_age_label(60), "1 分 00 秒前");
        assert_eq!(super::refresh_age_label(3661), "1 时 01 分前");
    }
}
