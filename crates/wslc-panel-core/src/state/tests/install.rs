//! 安装域：来源能力、计划、进度事件、在线清单与镜像测速。

use super::super::prelude::*;
use super::super::*;

#[test]
fn install_sources_are_listed_and_described() {
    for kind in InstallSourceKind::ALL {
        assert!(!kind.label().is_empty(), "{kind:?}");
        assert!(!kind.hint().is_empty(), "{kind:?}");
    }
    assert!(InstallSourceKind::ALL.contains(&InstallSourceKind::default()));
    // 默认必须是最不依赖网络的那条
    assert_eq!(InstallSourceKind::default(), InstallSourceKind::Local);
}

#[test]
fn install_source_capabilities_are_consistent() {
    // 只有在线安装有「装完启动」，也只有它不需要文件路径
    assert!(InstallSourceKind::Online.supports_launch());
    assert!(!InstallSourceKind::Online.needs_path());

    // 本地那一条要选文件，而且要有标签/占位/过滤器
    // （它把原来的 tar / VHDX / `.wsl` 三条合一了 —— 后缀由
    // `classify_local_file` 分岔，见 `wslc_core::model::install`）
    let kind = InstallSourceKind::Local;
    assert!(!kind.supports_launch(), "{kind:?}");
    assert!(kind.needs_path(), "{kind:?}");
    assert!(kind.is_local(), "{kind:?}");
    assert!(!kind.path_label().is_empty(), "{kind:?}");
    assert!(!kind.path_placeholder().is_empty(), "{kind:?}");
    assert!(kind.requires_install_dir(), "{kind:?}");
    let (label, extensions) = kind.file_filter().expect("本地来源要能弹选择器");
    assert!(!label.is_empty(), "{kind:?}");
    // 后缀表必须是**并集**：tar 系列 + vhdx + .wsl，少一个用户就选不到
    for expected in ["tar", "gz", "xz", "vhdx", "vhd", "wsl"] {
        assert!(
            extensions.contains(&expected),
            "少了 {expected}：{extensions:?}"
        );
    }

    // 两条"网络来源"不要文件路径，也不要文件选择器
    for kind in [InstallSourceKind::Mirror, InstallSourceKind::Online] {
        assert!(!kind.needs_path(), "{kind:?}");
        assert!(kind.file_filter().is_none(), "{kind:?}");
        assert!(!kind.is_local(), "{kind:?}");
        assert!(kind.is_online() == (kind == InstallSourceKind::Online));
    }
    // 镜像站要安装目录（下载完要 --import），在线安装可以留空
    assert!(InstallSourceKind::Mirror.is_mirror());
    assert!(InstallSourceKind::Mirror.requires_install_dir());
    assert!(!InstallSourceKind::Online.requires_install_dir());

    // 版本**不给用户选**：本项目只支持 WSL 2，安装命令里固定 `--version 2`
    // （`InstallSourceKind` 上再也没有 `supports_version` 这种东西了）。

    // 三种来源的标签不能重复（按钮上会分不清）
    let labels: Vec<&str> = InstallSourceKind::ALL.iter().map(|k| k.label()).collect();
    let unique: std::collections::HashSet<&&str> = labels.iter().collect();
    assert_eq!(unique.len(), labels.len(), "{labels:?}");
    assert_eq!(labels.len(), 3, "本地那三条已经合一：{labels:?}");

    // 「微软商店」必须**一眼看得出来**是商店 —— 用户最想找的就是它，
    // 之前它叫"在线安装"，从名字上完全看不出跟商店有关系。
    assert!(
        InstallSourceKind::Online.label().contains("微软商店"),
        "{}",
        InstallSourceKind::Online.label()
    );
    // 商店排在镜像源前面：商店是 WSL 官方那条路，镜像源是它的备选
    let order = InstallSourceKind::ALL.to_vec();
    let store = order.iter().position(|k| *k == InstallSourceKind::Online);
    let mirror = order.iter().position(|k| *k == InstallSourceKind::Mirror);
    assert!(store < mirror, "{order:?}");
}

#[test]
fn install_progress_follows_the_events() {
    use wslc_core::cmd::install::InstallEvent;

    let mut progress = InstallProgress::new("MyUbuntu", vec!["Ubuntu-24.04".to_owned()]);
    assert!(progress.is_running());
    assert_eq!(progress.percent(), None);

    // 第一步开始
    assert!(progress.apply(&InstallEvent::Step {
        index: 1,
        total: 3,
        label: "创建安装目录".to_owned(),
        line: "创建安装目录 D:\\wsl\\MyUbuntu".to_owned(),
        cancellable: true,
    }));
    assert_eq!(progress.index, 1);
    assert_eq!(progress.total, 3);
    assert!(progress.cancellable);
    assert!(progress.log.iter().any(|l| l.contains("第 1/3 步")));
    assert!(progress.log.iter().any(|l| l.contains("创建安装目录 D:")));

    // 进度事件：数字没变就不算变化（界面据此不重绘）
    assert!(progress.apply(&InstallEvent::Progress {
        have: 100,
        total: Some(400),
        secs: 5,
    }));
    assert!(!progress.apply(&InstallEvent::Progress {
        have: 100,
        total: Some(400),
        secs: 5,
    }));
    assert_eq!(progress.percent(), Some(25.0));
    assert!(progress.speed().is_some());

    // 换步骤要把上一步的进度归零（那是上一步的产物大小）
    progress.apply(&InstallEvent::Step {
        index: 2,
        total: 3,
        label: "导入".to_owned(),
        line: String::new(),
        // 重定位那一步不可取消 —— 界面据此不给"取消"按钮
        cancellable: false,
    });
    assert!(!progress.cancellable);
    assert_eq!(progress.have, 0);
    assert_eq!(progress.percent(), None);

    // 一行的输出同时进日志和 last_line
    progress.apply(&InstallEvent::Line("正在导入...".to_owned()));
    assert_eq!(progress.last_line, "正在导入...");

    // 步骤结束 → 记一笔
    progress.apply(&InstallEvent::StepDone {
        index: 2,
        ok: false,
        detail: "退出码 -1".to_owned(),
    });
    assert_eq!(progress.finished_count(), 1);
    assert!(!progress.finished[0].ok);
    assert!(progress.log.iter().any(|l| l.contains("✘")));

    // 结束之后不算"还在跑"
    assert!(progress.finish(InstallOutcome::Failed {
        detail: "x".to_owned(),
        step: Some("导入".to_owned()),
    }));
    assert!(!progress.is_running());
    // 同一个结局重复设置 → 不算变化
    assert!(!progress.finish(InstallOutcome::Failed {
        detail: "x".to_owned(),
        step: Some("导入".to_owned()),
    }));
}

#[test]
fn busy_with_also_knows_the_name_the_install_registers_first() {
    // 改名安装的中间态：`wsl -l -v` 上是**源名**（`Ubuntu-24.04`），
    // 而用户要的是 `MyUbuntu`。只认最终名字的话，那段时间里列表上那一行
    // 的「终止 / 删除 / 打开终端」全都可点 —— 而重定位正是
    // 「导出 → 注销 → 导入」那段不可逆的操作（见 `busy_with` 的说明）。
    let mut state = AppState::new(wslc_core::Wslc::new(), wslc_core::Wsl::new());

    // 还没开始装：谁都不拦
    assert_eq!(state.busy_with("Ubuntu-24.04"), None);

    state.installing = Some(InstallProgress::new(
        "MyUbuntu",
        vec!["Ubuntu-24.04".to_owned()],
    ));
    assert_eq!(state.busy_with("MyUbuntu"), Some("安装"));
    assert_eq!(state.busy_with("Ubuntu-24.04"), Some("安装"), "源名也要认");
    // 大小写不敏感（WSL 自己就不区分）
    assert_eq!(state.busy_with("ubuntu-24.04"), Some("安装"));
    // 别的发行版照旧放行 —— 安装要几十分钟，不能把整页锁住
    assert_eq!(state.busy_with("Debian"), None);

    // 装完之后不再拦
    if let Some(progress) = state.installing.as_mut() {
        progress.finish(InstallOutcome::Success("好了".to_owned()));
    }
    assert_eq!(state.busy_with("Ubuntu-24.04"), None);
    assert_eq!(state.busy_with("MyUbuntu"), None);
}

#[test]
fn install_progress_log_is_capped() {
    use wslc_core::cmd::install::InstallEvent;

    let mut progress = InstallProgress::new("X", Vec::new());
    for i in 0..(InstallProgress::MAX_LOG_LINES + 50) {
        progress.apply(&InstallEvent::Line(format!("第 {i} 行")));
    }
    assert_eq!(progress.log.len(), InstallProgress::MAX_LOG_LINES);
    // 留下的是**最后**那些行（用户要看的是最新的）
    assert!(
        progress
            .log
            .last()
            .unwrap()
            .contains(&format!("第 {} 行", InstallProgress::MAX_LOG_LINES + 49))
    );
}

#[test]
fn online_state_filters_and_selects() {
    let mut online = OnlineDistroState::new();
    assert!(online.filtered("").is_empty());
    assert_eq!(online.source, OnlineListSource::Unknown);
    assert!(online.source.label().is_empty());

    online.items = vec![
        OnlineDistro::new("Ubuntu", "Ubuntu"),
        OnlineDistro::new("Ubuntu-24.04", "Ubuntu 24.04 LTS"),
        OnlineDistro::new("Debian", "Debian GNU/Linux"),
    ];
    online.source = OnlineListSource::FallbackJson;
    assert_eq!(online.filtered("").len(), 3);
    assert_eq!(online.filtered("ubuntu").len(), 2);
    assert_eq!(online.filtered("24").len(), 1);

    online.select("Ubuntu-24.04");
    assert_eq!(online.selected.as_deref(), Some("Ubuntu-24.04"));

    // 拉取失败时要把旧清单清掉，否则用户会挑一个已经不成立的列表
    online.clear();
    assert!(online.items.is_empty());
    assert!(online.selected.is_none());
}

#[test]
fn online_ids_match_the_mirror_catalog() {
    fn offer(name: &str, version: &str) -> Offer {
        Offer {
            name: name.to_owned(),
            version: version.to_owned(),
            sources: vec![wslc_core::mirrors::OfferSource {
                mirror: "lxc-tuna".to_owned(),
                url: format!("https://mirrors.tuna.tsinghua.edu.cn/x/{name}-{version}.tar.xz"),
                format: "tar.xz".to_owned(),
            }],
        }
    }

    let mut state = MirrorState::new();
    // 名字与版本都照真实清单（wslui 那份）写
    state.set_offers(
        vec![
            offer("Ubuntu", "24.04"),
            offer("Ubuntu", "22.04"),
            offer("Debian", "13"),
            offer("Kali", "current"),
            offer("Arch Linux", "current"),
            offer("Fedora", "44"),
            offer("AlmaLinux", "9"),
            offer("openSUSE", "tumbleweed"),
        ],
        String::new(),
        None,
    );

    // ① 名字 + 版本拼起来对上
    assert_eq!(
        state.match_online_id("Ubuntu-24.04").as_deref(),
        Some("Ubuntu 24.04")
    );
    assert_eq!(
        state.match_online_id("ubuntu 22.04").as_deref(),
        Some("Ubuntu 22.04")
    );
    assert_eq!(
        state.match_online_id("AlmaLinux-9").as_deref(),
        Some("AlmaLinux 9")
    );
    assert_eq!(
        state.match_online_id("openSUSE-Tumbleweed").as_deref(),
        Some("openSUSE tumbleweed")
    );
    // ② 只对上名字（在线 id 不带版本）
    assert_eq!(
        state.match_online_id("Ubuntu").as_deref(),
        Some("Ubuntu 24.04")
    );
    assert_eq!(
        state.match_online_id("Debian").as_deref(),
        Some("Debian 13")
    );
    // ③ 名字前缀（两边叫法有出入）
    assert_eq!(
        state.match_online_id("kali-linux").as_deref(),
        Some("Kali current")
    );
    assert_eq!(
        state.match_online_id("archlinux").as_deref(),
        Some("Arch Linux current")
    );
    assert_eq!(
        state.match_online_id("FedoraLinux-44").as_deref(),
        Some("Fedora 44")
    );
    // 认不出来 / 空串 → 不硬猜
    assert_eq!(state.match_online_id("SUSE-Linux-Enterprise-15-SP7"), None);
    assert_eq!(state.match_online_id(""), None);
    assert_eq!(state.match_online_id("   "), None);
}

#[test]
fn a_bare_family_name_falls_back_to_its_newest_versioned_sibling() {
    // 就是本机的真实清单（顺序也照 `wsl -l -o`：裸名字在最前）
    let mut online = OnlineDistroState::new();
    online.items = vec![
        OnlineDistro::new("Ubuntu", "Ubuntu"),
        OnlineDistro::new("Ubuntu-26.04", "Ubuntu 26.04 LTS"),
        OnlineDistro::new("Ubuntu-24.04", "Ubuntu 24.04 LTS"),
        OnlineDistro::new("Ubuntu-22.04", "Ubuntu 22.04 LTS"),
        OnlineDistro::new("Debian", "Debian GNU/Linux"),
        OnlineDistro::new("kali-linux", "Kali Linux Rolling"),
        OnlineDistro::new("AlmaLinux-10", "AlmaLinux OS 10"),
        OnlineDistro::new("AlmaLinux-9", "AlmaLinux OS 9"),
    ];

    // 裸 `Ubuntu` → 最新的那个版本号（不是列表里的下一个）
    let none: Vec<String> = Vec::new();
    assert_eq!(
        online.sibling_id("Ubuntu", &none).as_deref(),
        Some("Ubuntu-26.04")
    );
    // 带版本的 id 自己不再往下找
    assert_eq!(online.sibling_id("Ubuntu-24.04", &none), None);
    assert_eq!(online.sibling_id("AlmaLinux-10", &none), None);
    // 没有兄弟的不硬猜
    assert_eq!(online.sibling_id("Debian", &none), None);
    assert_eq!(online.sibling_id("kali-linux", &none), None);
    assert_eq!(online.sibling_id("", &none), None);
    // 版本比较按数值而不是按字典序（22 < 24 < 26）
    assert_ne!(
        online.sibling_id("Ubuntu", &none).as_deref(),
        Some("Ubuntu-22.04")
    );

    // ⚠️ 已经装过的兄弟不能选：本机就是这个状态（`Ubuntu-26.04` 装着），
    // 换过去只会撞 `Wsl/InstallDistro/ERROR_ALREADY_EXISTS`（实测）。
    let taken = vec!["Ubuntu-26.04".to_owned()];
    assert_eq!(
        online.sibling_id("Ubuntu", &taken).as_deref(),
        Some("Ubuntu-24.04"),
        "最新的那个装过了，就退到下一个版本"
    );
    // 大小写不同也是同一个发行版
    let taken = vec!["ubuntu-26.04".to_owned()];
    assert_eq!(
        online.sibling_id("Ubuntu", &taken).as_deref(),
        Some("Ubuntu-24.04")
    );
    // 同族的全装过了 → 干脆不换（返回 None，让界面照实说这条路装不动）
    let taken = vec![
        "Ubuntu-26.04".to_owned(),
        "Ubuntu-24.04".to_owned(),
        "Ubuntu-22.04".to_owned(),
    ];
    assert_eq!(online.sibling_id("Ubuntu", &taken), None);
    // 装了别的发行版不影响
    let taken = vec!["Debian".to_owned()];
    assert_eq!(
        online.sibling_id("Ubuntu", &taken).as_deref(),
        Some("Ubuntu-26.04")
    );
}

#[test]
fn probe_results_are_sorted_fastest_first_with_failures_last() {
    // 探测是**按清单顺序**发起的（见 `Shell::probe_mirrors`），所以结果必须
    // 排一下 —— 否则界面上"哪个镜像快"要靠用户自己在十几行里找。
    fn result(site: &str, code: u16, secs: f64) -> MirrorProbeResult {
        MirrorProbeResult {
            site: site.to_owned(),
            url: format!("https://x/{site}"),
            code,
            secs,
            bytes: if code == 200 { Some(10) } else { None },
        }
    }

    let mut state = MirrorState::new();
    state.results = vec![
        result("慢", 200, 2.0),
        result("连不上", 0, 8.0),
        result("快", 200, 0.3),
        result("404", 404, 0.1),
        // ⚠️ 探测没跑成的候选：`code == 0` 且 `secs == 0.0`
        // （`probe_mirrors` 里那个 `unwrap_or(0.0)`）
        result("没探", 0, 0.0),
    ];
    state.sort_results_by_speed();

    let sites: Vec<&str> = state.results.iter().map(|r| r.site.as_str()).collect();
    assert_eq!(
        sites,
        vec!["快", "慢", "没探", "404", "连不上"],
        "可用的按耗时升序排前面，失败的排后面；\
             只按 `secs` 排的话「没探」（0.0）会跑到最前面，看着像最快的那个"
    );
}

#[test]
fn the_catalog_is_sorted_like_the_reference() {
    // 用户要求"在线发行版的清单照 wsl-dashboard 来"：Ubuntu 最前、Debian 次之，
    // 同族按版本**倒序**（新的在前），滚动版（没有版本号）排最后。
    fn offer(name: &str, version: &str) -> Offer {
        Offer {
            name: name.to_owned(),
            version: version.to_owned(),
            sources: vec![wslc_core::mirrors::OfferSource {
                mirror: "lxc-tuna".to_owned(),
                url: format!("https://x/{name}-{version}.tar.xz"),
                format: "tar.xz".to_owned(),
            }],
        }
    }

    let mut state = MirrorState::new();
    state.set_offers(
        vec![
            offer("openSUSE", "tumbleweed"),
            offer("Rocky Linux", "10"),
            offer("Ubuntu", "20.04"),
            offer("Debian", "12"),
            offer("openSUSE", "16.0"),
            offer("Ubuntu", "24.04"),
            offer("Debian", "13"),
            offer("Ubuntu", "22.04"),
            offer("Alpine", "3.22"),
            offer("openEuler", "24.03"),
        ],
        "测试清单".to_owned(),
        None,
    );

    let ids: Vec<String> = state.offers.iter().map(Offer::id).collect();
    assert_eq!(
        ids,
        vec![
            // 1 常用度：Ubuntu 最前，同族版本倒序
            "Ubuntu 24.04",
            "Ubuntu 22.04",
            "Ubuntu 20.04",
            // 2 Debian
            "Debian 13",
            "Debian 12",
            // 6 openSUSE：16.0 在滚动版前面（数值键 > 空键）
            "openSUSE 16.0",
            "openSUSE tumbleweed",
            // 9 / 11 / 12
            "Rocky Linux 10",
            "Alpine 3.22",
            "openEuler 24.03",
        ]
    );
    // 排完之后默认选中第一条 —— 参考实现里用户打开就是 Ubuntu 24.04
    assert_eq!(state.distro_id, "Ubuntu 24.04");
}

#[test]
fn mirror_state_forgets_previous_probes_when_switching_or_reloading() {
    fn offer(name: &str, version: &str) -> Offer {
        Offer {
            name: name.to_owned(),
            version: version.to_owned(),
            sources: vec![wslc_core::mirrors::OfferSource {
                mirror: "lxc-tuna".to_owned(),
                url: format!("https://mirrors.tuna.tsinghua.edu.cn/x/{name}-{version}.tar.xz"),
                format: "tar.xz".to_owned(),
            }],
        }
    }
    fn probed() -> MirrorProbeResult {
        MirrorProbeResult {
            site: "lxc-tuna".to_owned(),
            url: "https://mirrors.tuna.tsinghua.edu.cn/x/a.tar.xz".to_owned(),
            code: 200,
            secs: 0.5,
            bytes: Some(100),
        }
    }

    let mut state = MirrorState::new();
    // 还没拉清单 → 没有可选的
    assert!(state.selected_offer().is_none());
    assert!(state.distro_id.is_empty());

    state.set_offers(
        vec![offer("Ubuntu", "24.04"), offer("Alpine", "3.22")],
        "测试清单".to_owned(),
        Some("2026-10-09T00:00:00Z".to_owned()),
    );
    // 拉回来之后默认选第一条
    assert_eq!(state.distro_id, "Ubuntu 24.04");
    assert_eq!(state.origin, "测试清单");
    assert_eq!(state.updated.as_deref(), Some("2026-10-09T00:00:00Z"));

    state.results.push(probed());
    state.chosen = Some(MirrorChoice {
        site: "lxc-tuna".to_owned(),
        url: "https://mirrors.tuna.tsinghua.edu.cn/x/a.tar.xz".to_owned(),
        release: "24.04".to_owned(),
        format: "tar.xz".to_owned(),
        bytes: Some(100),
    });

    state.select_distro("Alpine 3.22");
    assert_eq!(state.distro_id, "Alpine 3.22");
    // 换了发行版就不该留着上一条的探测结果（那会拿着 A 的 URL 去装 B）
    assert!(state.results.is_empty());
    assert!(state.chosen.is_none());
    assert!(state.error.is_none());

    // 选同一个不算换
    state.results.push(probed());
    state.select_distro("Alpine 3.22");
    assert_eq!(state.results.len(), 1);

    // 重新拉清单：原来选的那条还在 → 保留选择与探测结果
    state.set_offers(
        vec![offer("Ubuntu", "24.04"), offer("Alpine", "3.22")],
        "新清单".to_owned(),
        None,
    );
    assert_eq!(state.distro_id, "Alpine 3.22");
    assert_eq!(state.results.len(), 1);

    // 原来选的那条**没了**（版本更新）→ 回落到第一条并清掉探测结果
    state.set_offers(vec![offer("Ubuntu", "26.04")], "新清单".to_owned(), None);
    assert_eq!(state.distro_id, "Ubuntu 26.04");
    assert!(state.results.is_empty() && state.chosen.is_none());

    // id 与清单对不上（理论上不该发生）时也要能回落到第一条，而不是给 None
    state.distro_id = "不存在的 1".to_owned();
    assert_eq!(
        state.selected_offer().map(Offer::id).as_deref(),
        Some("Ubuntu 26.04")
    );
}

#[test]
fn mirror_probe_summary_says_something_useful_for_every_outcome() {
    let ok = MirrorProbeResult {
        site: "清华 TUNA".to_owned(),
        url: "https://x".to_owned(),
        code: 200,
        secs: 1.234,
        bytes: Some(229_623_728),
    };
    assert!(ok.is_ok());
    let text = ok.summary();
    assert!(text.contains("229.6 MB"), "{text}");
    assert!(text.contains("1.23"), "{text}");

    // 连不上（curl 给 000）
    let dead = MirrorProbeResult {
        code: 0,
        secs: 8.0,
        ..ok.clone()
    };
    assert!(!dead.is_ok());
    assert!(dead.summary().contains("连不上"), "{}", dead.summary());

    // 404：文件可能改名了，这是最常见的一种失败
    let missing = MirrorProbeResult {
        code: 404,
        secs: 0.1,
        bytes: None,
        ..ok.clone()
    };
    assert!(missing.summary().contains("404"), "{}", missing.summary());

    // 200 但拿不到大小时也要能显示
    let unknown = MirrorProbeResult {
        bytes: None,
        ..ok.clone()
    };
    assert!(
        unknown.summary().contains("大小未知"),
        "{}",
        unknown.summary()
    );
}

#[test]
fn plan_context_reads_prefs_and_the_sparse_flag() {
    let mut state = AppState::new(wslc_core::Wslc::new(), wslc_core::Wsl::new());
    state.prefs.install_dir = Some(r"D:\wsl".to_owned());

    // 没读过 .wslconfig → 不开稀疏
    let ctx = state.plan_context();
    assert_eq!(ctx.default_dir.as_deref(), Some(r"D:\wsl"));
    assert!(!ctx.wslconfig_sparse);
    assert!(!ctx.temp_dir.is_empty());
    assert!(!ctx.stamp.is_empty());

    // 采集里读到的真实配置要求稀疏 → 跟着它走
    state.snapshot.wslconfig.text = Some("[experimental]\nsparseVhd=true\n".to_owned());
    assert!(state.plan_context().wslconfig_sparse);
}

#[test]
fn app_state_preflight_uses_the_current_distro_list() {
    use wslc_core::model::install::{InstallSource, InstallSpec};

    let mut state = AppState::new(wslc_core::Wslc::new(), wslc_core::Wsl::new());
    // 有默认安装目录 → tar 导入的必填项能由"默认目录 + 名字"推出来
    state.prefs.install_dir = Some(r"D:\wsl".to_owned());
    let spec = InstallSpec::new(
        "Ubuntu",
        InstallSource::Tar {
            path: r"D:\a.tar".to_owned(),
        },
    );

    // 列表是空的时候它能过（只差目录非空与否）
    assert!(state.preflight(&spec, false).ok());
    assert!(!state.preflight(&spec, true).ok());

    // 列表里已经有 Ubuntu → 报重名
    state.snapshot.distros.push(wslc_core::model::Distro::new(
        "Ubuntu",
        wslc_core::model::DistroState::Stopped,
        Some(2),
        false,
    ));
    assert_eq!(state.distro_names(), vec!["Ubuntu".to_owned()]);
    let check = state.preflight(&spec, false);
    assert!(!check.ok());
    assert!(check.error_text().unwrap().contains("已经有一个"));
}
