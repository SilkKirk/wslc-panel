//! 把图标与版本信息嵌进 `wslc-panel.exe`。
//!
//! # 为什么需要 build.rs
//!
//! exe 是一份 PE 文件；图标和 VERSIONINFO 都住在它的**资源段**里。
//! 不嵌资源的话，Windows 只能画一张白纸 —— 快捷方式、任务栏、Alt+Tab
//! 全都没有图标（实机截图里就是这样）。
//!
//! # 为什么走 `embed-resource` 而不是自己调 `rc.exe`
//!
//! `AGENTS.md` §1：**本机没有 Rust 工具链，也没有 Windows SDK** ——
//! 本机 `where rc.exe` 是空的。所以：
//!
//! - 本机无法验证任何 `rc.exe` 调用写对没有；
//! - 而 `embed-resource` 存在的全部理由就是"**替我们找** rc.exe 在哪"
//!   （它的文档原话：MSVC 的 rc.exe 不在 `$PATH` 里，于是它去翻最可能的位置）。
//!
//! 代价：它要给 `wslc-panel` 加一条 **build-dependency**，于是
//! `Cargo.lock` 里 `name = "wslc-panel"` 的 `dependencies` 数组会多一项 ——
//! 而所有 cargo 步骤都带 `--locked`。这一步是在本机手动改的锁文件
//! （`AGENTS.md` §3 允许，但要求按 name + 紧跟的 version 成对核对）。
//! 版本 `3.0.12` 本来就在锁里（gpui 的 build-dep 带的），所以只是**多一条边**。
//!
//! # 只在 Windows 上做
//!
//! 资源段是 PE 的概念。别的平台上这个 build script 什么都不干 ——
//! 否则 `cargo check` 在非 Windows 上会去找一个不存在的 `rc.exe`。

fn main() {
    // 资源文件或图标改了要重新编译（不然改了图看不见效果）。
    println!("cargo:rerun-if-changed=app.rc");
    println!("cargo:rerun-if-changed=../../assets/wslc-panel.ico");

    if !cfg!(target_os = "windows") {
        return;
    }

    let version = std::env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "0.0.0".to_owned());

    // VERSIONINFO 的 `FILEVERSION` 要的是**四个逗号分隔的数字**（`0,10,0,0`），
    // 不是点分字符串 —— 点分那个只给 `StringFileInfo` 里的 `FileVersion` 用。
    // 所以这里算两份。
    let numeric = file_version(&version);
    let display = version.clone();

    // 宏名对应 app.rc 里的 WSLC_PANEL_VERSION_*。
    // `KEY=VALUE` 这种形式是 embed-resource 的参数宏约定。
    let macros = [
        format!("WSLC_PANEL_VERSION_NUMERIC={numeric}"),
        format!("WSLC_PANEL_VERSION_STR={display}"),
    ];

    embed_resource::compile("app.rc", macros)
        // ⚠️ `manifest_optional()` 只管**清单（manifest）那块**：找不到清单文件
        // 就跳过，不报错。它**不**让"资源编译失败"变成静默降级 —— 那是我原先
        // 在这条注释里写错的（`embed-resource` 的文档语义是 manifest 可选，
        // 不是整个 .rc 可选；这一条是读 API 推出来的，没有源码可查，因为本机
        // 没有 cargo registry 缓存）。
        //
        // 所以下面的 `panic!` 是**真的会 panic**，而这是刻意的：
        // 资源编不出来时宁可让构建红，也不要出一个"没有图标但 CI 全绿"的 exe
        // （v0.10.0 就是这么发出去的）。release.yml 里那一步
        // `verify-exe-icon.ps1` 再兜一层 —— 它现在会真的比图标**内容**。
        .manifest_optional()
        .unwrap_or_else(|e| {
            panic!("嵌入资源失败：{e}（rc.exe 找不到？本机没有 Windows SDK 是正常的，CI 有）")
        });
}

/// 把 `0.10.0` 变成 `0,10,0,0`。
///
/// 三段补成四段；`-pre` / `+build` 后缀一律剥掉（那不是数字，rc.exe 会报语法错）。
/// 任何一段不是合法 `u16` 时退回 `0,0,0,0` —— 让构建继续，而不是死在一句
/// 看不懂的 rc.exe 语法错上（并留一条 cargo warning 说明发生了什么）。
fn file_version(version: &str) -> String {
    let core = version.split(['-', '+']).next().unwrap_or("0.0.0");
    let mut parts: Vec<String> = core.split('.').map(str::to_owned).collect();
    while parts.len() < 4 {
        parts.push("0".to_owned());
    }

    if parts.iter().take(4).all(|p| p.parse::<u16>().is_ok()) {
        parts[..4].join(",")
    } else {
        println!("cargo:warning=版本号 {version} 不是纯数字形式，VERSIONINFO 退回 0,0,0,0");
        "0,0,0,0".to_owned()
    }
}
