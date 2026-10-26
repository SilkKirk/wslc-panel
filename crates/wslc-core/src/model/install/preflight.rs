//! 装前检查：`Preflight` / `InstallFieldErrors` 与那套校验规则。

use super::*;

/// 装前检查的结果。
///
/// 分成 errors / warnings 两类：errors **挡住**提交（不发起任何进程 ——
/// 这类错误里最没意义的就是"跑一遍命令再告诉你参数不对"），
/// warnings 只是提醒（比如"会多一次全量拷贝"）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Preflight {
    /// 必须解决才能安装。
    pub errors: Vec<String>,
    /// 建议知道，但不挡。
    pub warnings: Vec<String>,
}

impl Preflight {
    /// 能不能提交。
    pub fn ok(&self) -> bool {
        self.errors.is_empty()
    }

    /// 一条能直接进提示条的消息（没有错误时给 `None`）。
    pub fn error_text(&self) -> Option<String> {
        if self.errors.is_empty() {
            None
        } else {
            Some(self.errors.join("；"))
        }
    }

    /// 把错误落到**具体输入框**上：界面据此给对应的框标红。
    ///
    /// 这里仍然按错误文案判断，而不是在 [`validate`] 里返回结构化的
    /// `(字段, 文案)` —— 后者要改所有 `out.push(...)` 的调用点，收益只是
    /// 省掉下面几行 `contains`；而这些文案本来就定义在本文件里，改文案时
    /// 顺手看一眼这里即可。真正的保证在测试 `field_errors_map_...`：
    /// 名字 / 文件或下载地址 / 安装目录三类错误必须各自落到正确的框上。
    pub fn field_errors(&self) -> InstallFieldErrors {
        let mut fields = InstallFieldErrors::default();
        for error in &self.errors {
            if error.contains("发行版名") || error.contains("已经有一个叫") {
                fields.name = true;
            }
            if error.contains("文件路径")
                || error.contains("下载地址")
                || error.contains("在线发行版")
            {
                fields.source = true;
            }
            if error.contains("安装目录") || error.contains("目录里已经有东西") {
                fields.install_dir = true;
            }
        }
        fields
    }
}

/// 装前检查里"哪几个输入框有问题"。
///
/// 只分三块：**发行版名**、**来源**（本地文件路径 / 在线发行版 / 镜像地址共用）、
/// **安装目录**。界面不需要知道具体是哪条文案 —— 点「开始安装」时它只负责把
/// 对应的框标红，并把焦点送过去；错误文案本身不再常驻页面。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct InstallFieldErrors {
    /// 发行版名（为空 / 非法 / 重名）。
    pub name: bool,
    /// 安装来源相关的输入（本地路径、在线发行版、镜像下载地址）。
    pub source: bool,
    /// 安装目录（为空但必填 / 相对路径 / 目录非空）。
    pub install_dir: bool,
}

impl InstallFieldErrors {
    /// 有没有任何一块需要标红。
    pub fn any(&self) -> bool {
        self.name || self.source || self.install_dir
    }
}

/// 装前检查（界面渲染和提交前都用它，保证"看到的"和"挡住的"是同一套规则）。
///
/// `taken` 是**已有的发行版名**（来自列表快照），`dir_non_empty` 由调用方
/// 用文件系统查出来（这里不碰磁盘）。
pub fn preflight(
    spec: &InstallSpec,
    ctx: &PlanContext,
    taken: &[String],
    dir_non_empty: bool,
) -> Preflight {
    let mut errors = validate(spec, ctx);
    let name = spec.name.trim();

    if !name.is_empty()
        && taken
            .iter()
            .any(|existing| crate::model::distro::same_name(existing, name))
    {
        errors.push(format!(
            "已经有一个叫「{name}」的发行版了 —— 换个名字，或者先把旧的删掉（本程序不会替你删）"
        ));
    }
    if dir_non_empty {
        errors.push(
            "这个安装目录里已经有东西了。换一个空目录 —— WSL 会在里面建自己的文件，\
             和别的东西混在一起不好收拾"
                .to_owned(),
        );
    }

    let mut warnings = Vec::new();
    if let InstallSource::Online {
        id, web_download, ..
    } = &spec.source
    {
        let id = id.trim();
        if !id.is_empty() && id != name {
            warnings.push(format!(
                "WSL 只认在线清单里的名字，所以会先装成「{id}」，再导出/注销/导入改成「{name}」——\
                 多一次全量拷贝，需要额外的临时空间。"
            ));
        }
        if *web_download {
            warnings.push(
                "`--web-download` 是从网络下载而不是走微软商店；网络不通时这条会失败，\
                 可以切回商店再试。"
                    .to_owned(),
            );
        }
    }
    if let InstallSource::Mirror { release, .. } = &spec.source {
        if !release.trim().is_empty() {
            warnings.push(format!(
                "文件会先下载到临时目录（{release}，几百 MB），装完自动删掉。"
            ));
        }
        warnings.push(
            "清单里给的是各个镜像站上的地址，会随版本更新而变化。\
             如果探测/下载报 404，说明那一版的文件改名了 —— 刷新清单、换一个版本，\
             或者用「自定义下载地址」。"
                .to_owned(),
        );
    }

    Preflight { errors, warnings }
}

/// 纯参数校验（不含"重名"和"目录非空"这两件需要外部信息的事）。
pub(crate) fn validate(spec: &InstallSpec, ctx: &PlanContext) -> Vec<String> {
    let mut out = Vec::new();
    let name = spec.name.trim();
    out.extend(name_syntax_errors(name));

    let dir = spec.effective_install_dir(ctx);

    match &spec.source {
        InstallSource::Tar { path } => {
            if path.trim().is_empty() {
                out.push("tar 文件路径不能为空".to_owned());
            }
            if dir.is_none() {
                out.push("从 tar 导入必须指定安装目录".to_owned());
            }
        }
        InstallSource::Vhdx { path } => {
            if path.trim().is_empty() {
                out.push("VHDX 文件路径不能为空".to_owned());
            }
            if dir.is_none() {
                out.push("从 VHDX 导入必须指定安装目录".to_owned());
            }
        }
        InstallSource::File { path } => {
            if path.trim().is_empty() {
                out.push("安装文件路径不能为空".to_owned());
            }
        }
        InstallSource::Online { id, .. } => {
            let id = id.trim();
            if id.is_empty() {
                out.push("请先选一个在线发行版（或手输它的名字）".to_owned());
            } else if !id.chars().all(is_name_char) {
                out.push(format!(
                    "在线发行版的名字里有非法字符：{id}（只允许 A-Z a-z 0-9 . _ -）"
                ));
            } else if id != name && dir.is_none() {
                out.push(format!(
                    "要把它改名成「{name}」，就必须给一个安装目录（重定位那一步要用）"
                ));
            }
        }
        InstallSource::Mirror { url, .. } => {
            let url = url.trim();
            if url.is_empty() {
                out.push(
                    "还没有选好下载地址 —— 清单一到就会**自动测速**挑最快的镜像，\
                     稍等几秒；也可以直接在下面手填一个下载地址"
                        .to_owned(),
                );
            } else if !crate::mirrors::is_http_url(url) {
                // 判据复用 `mirrors::is_http_url`：这个字符串会作为**裸 argv**
                // 交给 `curl.exe`，只判"以 http 开头"是不够/不一致的
                // （`-K<file>` 这类以 `-` 开头的东西必须一起挡掉）。
                out.push(format!("下载地址要以 http:// 或 https:// 开头：{url}"));
            }
            if dir.is_none() {
                out.push("从镜像源安装必须指定安装目录".to_owned());
            }
        }
    }

    // 安装目录给了就必须是绝对路径。
    //
    // ⚠️ 这里查的是 `effective_install_dir` 的结果：用户留空、由默认目录推出来的
    // 那个路径同样要过这一关 —— 默认目录本身可能是相对的。
    if let Some(dir) = dir.as_deref() {
        let dir = dir.trim();
        if !is_absolute_windows_path(dir) {
            out.push(format!(
                "安装目录要写成绝对路径（如 D:\\wsl\\MyDistro）：{dir}"
            ));
        }
    }

    out
}
