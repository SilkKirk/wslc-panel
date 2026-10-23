//! SQL 自动补全的 **GPUI 适配层**（v0.12.4）。
//!
//! # 这一层薄得只剩"格式转换"
//!
//! 该给哪些候选、替换哪一段、别名怎么认 —— 全在
//! [`wslc_panel_core::sqlcomplete`]（纯逻辑，`core` job 有 17 条单测）。
//! 这里只做一件事：把那个结果包成 `lsp_types::CompletionItem`，
//! 因为 `gpui-kit` 的 `CompletionProvider` trait 签名就是这么定的。
//!
//! 这么分的理由和 `sql_table.rs` 一样：**判断错了在界面上看不出来**
//! （补全列表里东西不对，用户只会觉得"这功能不好用"），
//! 所以判断必须能脱离 GPUI 断言。
//!
//! # 为什么直接依赖 `lsp-types`
//!
//! `gpui-kit` 只 re-export 了 `Position`（见 `component::input`），
//! `CompletionItem` / `CompletionResponse` 这些得自己 `use`。
//! 版本 0.97 是锁文件里**已有**的（`gpui-base` 的传递依赖），不引入新 crate。

use std::rc::Rc;
use std::sync::Arc;

use gpui_kit::component::input::{CompletionProvider, Rope, RopeExt};
use gpui_kit::*;
use lsp_types::{
    CompletionContext, CompletionItem, CompletionItemKind, CompletionResponse, CompletionTextEdit,
    InsertReplaceEdit, Range,
};

use wslc_core::model::schematree::SchemaTree;
use wslc_panel_core::sqlcomplete::{self, CandidateKind, Dialect};

/// 把我们的候选类别映射成 LSP 的类别（决定补全菜单里那个小图标）。
fn kind_of(kind: CandidateKind) -> CompletionItemKind {
    match kind {
        // 关键字就是关键字
        CandidateKind::Keyword => CompletionItemKind::KEYWORD,
        // 库 ≈ 模块（一堆表的容器）
        CandidateKind::Database => CompletionItemKind::MODULE,
        // 表 ≈ 结构体（有字段的东西）
        CandidateKind::Table => CompletionItemKind::STRUCT,
        // 列就是字段
        CandidateKind::Column => CompletionItemKind::FIELD,
    }
}

/// SQL 补全的提供者。
///
/// 持有一份**快照**（`Arc<SchemaTree>`）：补全在按键时同步跑，
/// 不能去 `Shell` 里现取（那要 `Context`），也不该每次按键克隆 800 个列名。
pub struct SqlCompletionProvider {
    tree: Arc<SchemaTree>,
    dialect: Dialect,
}

impl SqlCompletionProvider {
    pub fn new(tree: Arc<SchemaTree>, dialect: Dialect) -> Rc<Self> {
        Rc::new(Self { tree, dialect })
    }
}

impl CompletionProvider for SqlCompletionProvider {
    fn completions(
        &self,
        rope: &Rope,
        offset: usize,
        _trigger: CompletionContext,
        _window: &mut Window,
        _cx: &mut App,
    ) -> Task<Result<CompletionResponse>> {
        // 整段文本交给纯逻辑层判断（它只读，不改）。
        //
        // ⚠️ 这里**没有**用 `trigger` 做过滤：`gpui-kit` 的示例是靠
        // "触发字符"决定补不补（敲 `/` 才出候选）。我们的场景正相反 ——
        // 用户敲字母就该补，所以关键字/前缀的判断全在 `sqlcomplete::complete`
        // 里做（它会按前缀筛），不该在这一层再挡一道。
        let text = rope.to_string();
        let done = sqlcomplete::complete(&self.tree, self.dialect, &text, offset);

        // 一条候选都没有就返回空数组：组件库见到空数组就不弹菜单，
        // 而不是弹一个空框。
        if done.is_empty() {
            return Task::ready(Ok(CompletionResponse::Array(Vec::new())));
        }

        // 替换范围 = `[前缀起点, 光标)`。
        //
        // ⚠️ 必须是**前缀**而不是整行：整行替换会把
        // `SELECT … WHERE sel` 前面已经写好的语句吃掉。
        let start = rope.offset_to_position(done.replace_start);
        let end = rope.offset_to_position(offset);
        let range = Range::new(start, end);

        let items: Vec<CompletionItem> = done
            .items
            .iter()
            .map(|c| CompletionItem {
                label: c.label.clone(),
                kind: Some(kind_of(c.kind)),
                // 右侧那句灰字（"表" / "列" / "关键字"）
                detail: Some(c.kind.detail().to_owned()),
                // `InsertAndReplace`：两段范围一样，等于"整段替换"。
                // 用它而不是 `insert_text`，因为我们需要指定**替换哪一段**。
                text_edit: Some(CompletionTextEdit::InsertAndReplace(InsertReplaceEdit {
                    new_text: c.label.clone(),
                    insert: range,
                    replace: range,
                })),
                insert_text: None,
                ..Default::default()
            })
            .collect();

        Task::ready(Ok(CompletionResponse::Array(items)))
    }

    /// 什么情况下**弹出**补全菜单。
    ///
    /// `gpui-kit` 的示例实现得很宽松（一律 `true`）。我们收窄一点，
    /// 只在"正在打一个标识符"时弹：
    ///
    /// - 字母 / 数字 / `_` / `$`：正常打字，该补；
    /// - `.`：刚打完 `别名.`，正是要列列名的时候；
    /// - `` ` ``：反引号引用标识符，也该接着补；
    /// - **空字符串**：手动触发（`Ctrl+Space` 之类），一律弹。
    ///
    /// 空格、逗号、`)`、`;` 这些**不弹** —— 在那些位置弹一个补全菜单，
    /// 用户只是刚写完一个表达式，菜单只会挡住他看结果。
    fn is_completion_trigger(&self, _offset: usize, new_text: &str, _cx: &mut App) -> bool {
        if new_text.is_empty() {
            return true;
        }
        new_text
            .chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == '$' || c == '.' || c == '`')
    }
}
