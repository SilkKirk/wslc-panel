//! 部署域的纯逻辑测试（从 deploy.rs 原样搬过来，未改一条断言）。

use std::path::{Path, PathBuf};

use super::*;
use crate::model::{ContainerInspect, ContainerListItem};
use serde_json::json;

fn item(image: &str, command: &str) -> ContainerListItem {
    // 走真实的 serde 路径构造，而不是手搓结构体 —— 夹具的字段名必须和
    // `wslc list` 真给的一致（见 `docs/wslc-schema.md`）。
    let value = json!({
        "Id": "68da9a6b0012978d72fb1553c45797e05671b8d70b1e002d3a72cd0deb7a57fd",
        "Image": image,
        "Command": command,
        "Created": "2026-10-14T08:55:52Z",
        "State": "running",
        "Status": "Up 2 seconds",
        "Name": "nginx",
    });
    serde_json::from_value(value).expect("夹具应当能解析")
}

// -----------------------------------------------------------------------
// 识别
// -----------------------------------------------------------------------

#[test]
fn recognizes_runtimes_by_image_name() {
    assert_eq!(runtime_from_image("nginx"), Some(AppRuntime::Nginx));
    assert_eq!(
        runtime_from_image("nginx:1.27-alpine"),
        Some(AppRuntime::Nginx)
    );
    assert_eq!(
        runtime_from_image("openresty/openresty"),
        Some(AppRuntime::Nginx)
    );

    assert_eq!(runtime_from_image("openjdk"), Some(AppRuntime::Java));
    assert_eq!(
        runtime_from_image("eclipse-temurin:21-jre"),
        Some(AppRuntime::Java)
    );
    assert_eq!(runtime_from_image("amazoncorretto"), Some(AppRuntime::Java));
    assert_eq!(runtime_from_image("tomcat"), Some(AppRuntime::Java));

    assert_eq!(runtime_from_image("node"), Some(AppRuntime::Node));
    assert_eq!(runtime_from_image("node:20-slim"), Some(AppRuntime::Node));

    assert_eq!(runtime_from_image("golang"), Some(AppRuntime::Go));
    assert_eq!(runtime_from_image("golang:1.23"), Some(AppRuntime::Go));
}

#[test]
fn image_name_with_registry_prefix_still_matches() {
    // ⚠️ 这条是**本机那个真容器的实测值**：带上 registry 前缀之后
    // 必须还能认出 `nginx`，否则本机这个容器上根本不会出现按钮。
    assert_eq!(
        runtime_from_image("docker.1panel.live/library/nginx:latest"),
        Some(AppRuntime::Nginx)
    );
    // 带端口的 registry 主机名不能把端口当 tag 吃掉
    assert_eq!(
        runtime_from_image("localhost:5000/golang"),
        Some(AppRuntime::Go)
    );
    // 摘要形式（合法引用，但 `registry::split_input` 会拒它 —— 所以这里
    // 刻意不走那个函数）
    assert_eq!(
        runtime_from_image("alpine@sha256:abc"),
        None,
        "认不出来就是认不出来，不能报错"
    );
}

#[test]
fn unknown_images_are_not_guessed() {
    for image in ["mysql", "alpine", "redis", "postgres", "hello-world", ""] {
        assert_eq!(
            runtime_from_image(image),
            None,
            "{image} 不该被认成能部署的应用"
        );
    }
}

#[test]
fn recognizes_runtimes_by_command() {
    assert_eq!(
        runtime_from_command(r#"nginx -g "daemon off;""#),
        Some(AppRuntime::Nginx)
    );
    assert_eq!(
        runtime_from_command("java -jar /app/app.jar"),
        Some(AppRuntime::Java)
    );
    assert_eq!(
        runtime_from_command("node server.js"),
        Some(AppRuntime::Node)
    );
    assert_eq!(runtime_from_command("npm start"), Some(AppRuntime::Node));
    // Go **刻意不猜**：二进制的名字是任意的，启动命令里没有线索
    assert_eq!(runtime_from_command("./myapp"), None);
    assert_eq!(runtime_from_command("sleep infinity"), None);
}

#[test]
fn java_wins_over_node_when_both_appear() {
    // `java -jar node-exporter.jar` —— `-jar` 是强信号，别被 "node" 抢走
    assert_eq!(
        runtime_from_command("java -jar node-exporter.jar"),
        Some(AppRuntime::Java)
    );
}

#[test]
fn list_heuristic_prefers_image_over_command() {
    let by_image = container_app_kind(&item("nginx", "./something")).unwrap();
    assert_eq!(by_image.runtime, AppRuntime::Nginx);
    assert_eq!(by_image.signal, DeploySignal::Image);

    let by_command = container_app_kind(&item("my-company/web", "node server.js")).unwrap();
    assert_eq!(by_command.runtime, AppRuntime::Node);
    assert_eq!(by_command.signal, DeploySignal::Command);

    assert!(container_app_kind(&item("mysql", "mysqld")).is_none());
}

#[test]
fn the_real_nginx_container_is_recognized() {
    // ⚠️ 两个字段值都是从**本机那个真容器**上抄下来的（`wslc list`）。
    // 注意 `Command` 被截断成 20 字符 —— 所以能认出它**全靠镜像名**，
    // 这正是"没有端口依据也够用"的证据。
    let real = item(
        "docker.1panel.live/library/nginx:latest",
        "\"/docker-entrypoint.…\"",
    );
    let found = container_app_kind(&real).expect("真容器必须认得出");
    assert_eq!(found.runtime, AppRuntime::Nginx);
    assert_eq!(found.signal, DeploySignal::Image);
}

#[test]
fn inspect_recognizes_by_env_when_image_name_is_useless() {
    let insp = ContainerInspect::new(json!({
        "Config": {
            "Image": "my-company/thing:1",
            "Env": ["PATH=/usr/bin", "NGINX_VERSION=1.31.6", "NJS_VERSION=1.0.1"],
        }
    }));
    assert_eq!(runtime_from_inspect(&insp), Some(AppRuntime::Nginx));
}

#[test]
fn inspect_uses_untruncated_command() {
    // `inspect` 的 Cmd **没有**被截断 —— 所以这条比列表那条启发式强
    let insp = ContainerInspect::new(json!({
        "Config": {
            "Image": "my-company/totally-unknown:1",
            "Cmd": ["nginx", "-g", "daemon off;"],
        }
    }));
    assert_eq!(runtime_from_inspect(&insp), Some(AppRuntime::Nginx));
}

#[test]
fn inspect_of_a_real_nginx_container_recognizes_it() {
    // 字段是从本机 `wslc inspect nginx` 的真实输出里摘的（原样，没改）。
    let insp = ContainerInspect::new(json!({
        "Config": {
            "Cmd": ["nginx", "-g", "daemon off;"],
            "Entrypoint": ["/docker-entrypoint.sh"],
            "Env": [
                "PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin",
                "NGINX_VERSION=1.31.6",
                "NJS_VERSION=1.0.1",
                "NJS_RELEASE=1~trixie",
                "ACME_VERSION=0.4.1",
                "PKG_RELEASE=1~trixie"
            ],
            "Image": "docker.1panel.live/library/nginx:latest"
        },
        "Mounts": [
            {
                "Destination": "/usr/share/nginx/html",
                "Name": "webdata",
                "ReadWrite": true,
                "Source": "webdata",
                "Type": "volume"
            }
        ],
        "Ports": { "80/tcp": [ { "HostIp": "127.0.0.1", "HostPort": "80" } ] }
    }));
    assert_eq!(runtime_from_inspect(&insp), Some(AppRuntime::Nginx));
}

// -----------------------------------------------------------------------
// Web 根
// -----------------------------------------------------------------------

#[test]
fn picks_the_volume_mounted_under_the_nginx_web_root() {
    // 本机那个容器的实测结构
    let insp = ContainerInspect::new(json!({
        "Mounts": [
            { "Type": "volume", "Name": "webdata", "Source": "webdata",
              "Destination": "/usr/share/nginx/html", "ReadWrite": true }
        ]
    }));
    assert_eq!(
        nginx_web_root(&insp).as_deref(),
        Some("/usr/share/nginx/html")
    );
}

#[test]
fn web_root_keeps_the_site_mount_when_a_config_mount_is_also_there() {
    let insp = ContainerInspect::new(json!({
        "Mounts": [
            { "Type": "bind", "Source": "C:\\conf", "Destination": "/etc/nginx/conf.d" },
            { "Type": "volume", "Name": "web", "Destination": "/usr/share/nginx/html" }
        ]
    }));
    assert_eq!(
        nginx_web_root(&insp).as_deref(),
        Some("/usr/share/nginx/html")
    );
}

/// ⚠️ 回归钉（v0.9.13）：`/etc/nginx` 底下的挂载点**不是站点根**。
///
/// 早先这里会返回 `/etc/nginx/conf.d`，于是「添加部署」把配置目录预填成
/// 「容器内目标目录」——文件铺进去没有任何效果（那是配置，不是站点），
/// 而删的时候还会先删掉容器里同名的 `.conf`。见 [`nginx_web_root`] 那段。
#[test]
fn config_mounts_are_never_treated_as_the_web_root() {
    for destination in [
        "/etc/nginx",
        "/etc/nginx/conf.d",
        "/etc/nginx/conf.d/",
        "/usr/share/nginx", // 站点目录的**父目录**：往里铺文件也没人读
    ] {
        let insp = ContainerInspect::new(json!({
            "Mounts": [ { "Type": "volume", "Name": "conf", "Destination": destination } ]
        }));
        assert_eq!(
            nginx_web_root(&insp),
            None,
            "{destination} 不该被当成站点根（配置 / 父目录），要回落默认值"
        );
    }
}

#[test]
fn no_mounts_means_no_web_root_so_caller_uses_the_default() {
    assert_eq!(nginx_web_root(&ContainerInspect::new(json!({}))), None);
    // 形状不对的行**跳过**，不 panic
    let broken = ContainerInspect::new(json!({
        "Mounts": [ { "Type": "volume" }, "不是对象", 42 ]
    }));
    assert_eq!(nginx_web_root(&broken), None);
}

// -----------------------------------------------------------------------
// shell 引号
// -----------------------------------------------------------------------

#[test]
fn quotes_plain_values() {
    assert_eq!(shell_quote("/opt/app"), "'/opt/app'");
    assert_eq!(shell_quote(""), "''");
    assert_eq!(shell_quote("a b"), "'a b'");
}

#[test]
fn escapes_single_quotes_the_posix_way() {
    assert_eq!(shell_quote("it's"), r"'it'\''s'");
    // 引号在最前 / 最后也要对
    assert_eq!(shell_quote("'"), r"''\'''");
}

#[test]
fn injection_shapes_are_neutralized() {
    // 这几种形状如果没引号，就是真的命令执行
    let cases = [
        "; rm -rf /",
        "$(cat /etc/passwd)",
        "`id`",
        "a\nrm -rf /",
        "x' ; rm -rf / ; '",
        "&& curl evil.example",
        "${IFS}",
        "*",
        "~",
    ];
    for case in cases {
        let quoted = shell_quote(case);
        assert!(
            quoted.starts_with('\'') && quoted.ends_with('\''),
            "{quoted}"
        );
        // ⚠️ 判据：**剥掉外层引号之后**，内部不允许出现裸单引号 ——
        // 每个 `'` 都必须以 `'\''` 这个四字符组的形式出现。
        //
        // （第一版这条断言是错的：它在**没剥外层**的串上找 `'\''`，
        // 于是外层那两个引号永远算成"裸的"，九个用例**全都**断言失败。
        // 单测自己写错，比不写还坏 —— 它会让"红"变成噪音。）
        let inner = &quoted[1..quoted.len() - 1];
        let escaped = inner.matches(r"'\''").count();
        assert_eq!(
            inner.matches('\'').count(),
            escaped * 3,
            "内部只允许 `'\\''` 这一种转义：{quoted}"
        );
    }
}

// -----------------------------------------------------------------------
// 应用名白名单
// -----------------------------------------------------------------------

#[test]
fn accepts_reasonable_app_names() {
    for name in ["site", "my-app", "my_app", "app1", "A", &"x".repeat(64)] {
        assert!(valid_app_name(name), "{name} 应当合法");
    }
}

#[test]
fn rejects_dangerous_or_odd_app_names() {
    for name in [
        "",
        ".",
        "..",
        "../etc",
        "a/b",
        "a b",
        "a;b",
        "a$b",
        "中文",
        &"x".repeat(65),
    ] {
        assert!(!valid_app_name(name), "{name:?} 应当被拒");
    }
}

// -----------------------------------------------------------------------
// 脚本
// -----------------------------------------------------------------------

#[test]
fn prepare_target_removes_only_our_own_names() {
    let members = vec!["ruoyi-admin.jar".to_owned()];
    assert_eq!(
        prepare_target_script("/opt/app", true, &members),
        "mkdir -p '/opt/app' && rm -rf '/opt/app/ruoyi-admin.jar' \
         '/opt/app/app.log' '/opt/app/app.pid'"
    );
    // 目录不存在时 `mkdir -p` 要先建出来（`cp` 要求目标已存在）
    assert!(prepare_target_script("/opt/app", true, &members).starts_with("mkdir -p "));
    // ⚠️ **一个 `-delete` 都不许有**：整目录清空就是 v0.9.8 删掉 JDK 的那一手
    assert!(!prepare_target_script("/opt/app", true, &members).contains("-delete"));
}

/// ⚠️ 回归钉（v0.9.10）：容器重建过、存档里那条记录还在时，
/// 「重新部署」**也不许**碰目录里不是我们铺的东西。
///
/// 场景就是现场那次：目标目录 `/opt/java` 里躺着镜像自带的 `openjdk/`，
/// 而我们要铺的只有 `ruoyi-admin.jar` —— 删除列表里**绝不能**出现
/// `openjdk`，也绝不能出现 `find … -delete`。
#[test]
fn removals_never_touch_files_that_are_not_ours() {
    let members = vec!["ruoyi-admin.jar".to_owned()];
    for script in [
        prepare_target_script("/opt/java", true, &members),
        remove_script("/opt/java", &members),
    ] {
        assert!(script.contains("'/opt/java/ruoyi-admin.jar'"), "{script}");
        assert!(!script.contains("openjdk"), "{script}");
        assert!(!script.contains("-delete"), "{script}");
        // 目录本身永远不删（它可能是挂载点，`rm -rf` 会返回 1）
        assert!(!script.contains("rm -rf '/opt/java'"), "{script}");
    }
}

/// 成员名里带路径 / `..` 时**跳过**：宁可少删，也不能删到目标目录外面去。
#[test]
fn removals_skip_unsafe_member_names() {
    let members = vec![
        "ok.txt".to_owned(),
        "../escape.txt".to_owned(),
        "a/b.txt".to_owned(),
        "a\\b.txt".to_owned(),
        String::new(),
        "   ".to_owned(),
    ];
    let script = removals_script("/opt/app", &members);
    assert!(script.contains("'/opt/app/ok.txt'"), "{script}");
    assert!(!script.contains("escape"), "{script}");
    assert!(!script.contains("a/b"), "{script}");
    assert!(!script.contains("a\\b"), "{script}");
    // 固定那两个（日志 / pidfile）永远在删除列表里
    assert!(script.contains("'/opt/app/app.log'"), "{script}");
    assert!(script.contains("'/opt/app/app.pid'"), "{script}");
}

/// ⚠️ 回归钉（v0.9.11）：**第一次往非空目录铺时不再拒绝**。
///
/// v0.9.9 是拒绝的（`exit 0` + 哨兵）。逐名删除（v0.9.10）之后那层拒绝
/// 拦的已经不是危险，而是用户自己的正常部署 —— 实测：删掉旧记录 + 重建容器
/// 之后重新部署 `/opt/java`（那次完全安全）被直接拒绝，用户只能来问为什么。
#[test]
fn prepare_target_warns_but_still_proceeds_on_a_foreign_directory() {
    let members = vec!["ruoyi-admin.jar".to_owned()];
    let script = prepare_target_script("/opt/java", false, &members);
    // 目录非空会打哨兵 —— 调用方拿它去**说一句**（不是拿去报错）
    assert!(script.contains(TARGET_NOT_EMPTY), "{script}");
    assert!(script.contains("ls -A"), "{script}");
    // ⚠️ 哨兵**不许**带 `exit`：带了就是"拒绝部署"（v0.9.9 的行为，已废）
    assert!(!script.contains("exit"), "{script}");
    // 而且照样建目录、照样删我们自己的名字（= 照样往下走）
    assert!(script.contains("mkdir -p '/opt/java'"), "{script}");
    assert!(script.contains("'/opt/java/ruoyi-admin.jar'"), "{script}");
    // 哨兵排在动手之前（读起来是"先看一眼，再动手"）
    assert!(
        script.find(TARGET_NOT_EMPTY).unwrap() < script.find("rm -rf").unwrap(),
        "{script}"
    );
    // `ours = true` 时连那一句都不说（我们自己铺的目录，没什么好提醒的）
    assert!(!prepare_target_script("/opt/java", true, &members).contains("ls -A"));
}

#[test]
fn prepare_target_never_rm_rf_s_the_target_itself() {
    // ⚠️ 这条是回归闸：目标可能是**卷挂载点**（nginx 的 Web 根就是），
    // 而对挂载点 `rm -rf` 返回 **1**（`Device or resource busy`）——
    // 写成 `rm -rf <目标> && …` 的话 `&&` 链会断在第一步。
    // 实测踩到过。
    let members = vec!["index.html".to_owned()];
    for target in ["/opt/app", "/usr/share/nginx/html"] {
        // 两种 `ours` 都要查：守卫那段也不许出现 `rm -rf <目标>`
        for ours in [true, false] {
            let script = prepare_target_script(target, ours, &members);
            assert!(
                !script.contains(&format!("rm -rf '{target}'")),
                "不能对目标目录本身 rm -rf：{script}"
            );
            // 删的是**里面的名字**，而且没有整目录清空那一手
            assert!(
                script.contains(&format!("rm -rf '{target}/index.html'")),
                "{script}"
            );
            assert!(!script.contains("-delete"), "{script}");
        }
    }
}

#[test]
fn chmod_exec_bit_script_quotes_the_path() {
    assert_eq!(chmod_exec_script("/opt/app/bin"), "chmod +x '/opt/app/bin'");
    // 注入形状必须被引号吃掉
    let injected = chmod_exec_script("/opt/a; rm -rf /");
    assert!(injected.contains("'/opt/a; rm -rf /'"), "{injected}");
}

#[test]
fn copy_members_takes_a_file_as_is_and_a_directory_by_its_entries() {
    let file = PathBuf::from(r"E:\build\myapp.jar");
    assert_eq!(
        copy_members(&file, SourceKind::File, &[]),
        vec![file.clone()],
        "文件来源只复制它自己"
    );

    // ⚠️ 目录来源要**逐个顶层条目**（`wslc container cp` 复制目录时会把
    // 目录名一起带过去，整目录复制会让目标里多一层）
    let dir = PathBuf::from(r"E:\build\site");
    let entries = vec![
        dir.join("index.html"),
        dir.join("assets"),
        dir.join(".hidden"),
    ];
    assert_eq!(
        copy_members(&dir, SourceKind::Directory, &entries),
        entries,
        "目录来源要的就是那些顶层条目（含隐藏文件）"
    );
    // 空目录 → 没有东西可传（调用方要能把这件事说出来，而不是当作成功）
    assert!(copy_members(&dir, SourceKind::Directory, &[]).is_empty());
}

#[test]
fn cp_args_put_positionals_last_and_use_the_container_colon_form() {
    let args = cp_args(Path::new(r"E:\build\site\index.html"), "nginx", "/opt/site");
    assert_eq!(
        args,
        vec![
            "container",
            "cp",
            "-q",
            r"E:\build\site\index.html",
            "nginx:/opt/site"
        ]
    );
    // `container cp` 是**子命令**，不是 `cp`（顶层表里没有 `cp`，实测
    // `wslc cp` 报「无法识别的命令」）
    assert_eq!(args[0], "container");
    assert_eq!(args[1], "cp");
}

#[test]
fn start_script_groups_the_background_job() {
    let script = start_script("/opt/app", "java -jar app.jar");
    // ⚠️ 大括号必须在：`mkdir && cd && nohup … & echo $! > pid` 里
    // `&` 优先级最低，不分组就会把 pidfile 的写入甩到后台组外面去
    // （实测报 `cannot create …: Directory nonexistent`）。
    assert!(script.contains("&& { nohup "), "{script}");
    assert!(script.contains("& echo $! > "), "{script}");
    assert!(script.ends_with("; }"), "{script}");
    // 顺序也不能反：pidfile 必须在 `cd` 之后写，且 `$!` 紧跟 `&`
    let amp = script.find(" & echo $!").expect("要有 `& echo $!`");
    let brace = script.find("{ nohup").expect("要有分组");
    assert!(brace < amp, "{script}");
}

#[test]
fn start_script_quotes_paths_but_not_the_command() {
    let script = start_script("/opt/my app", "node server.js --flag=1");
    // 路径是数据 → 引号
    assert!(script.contains("mkdir -p '/opt/my app'"), "{script}");
    assert!(script.contains("> '/opt/my app/app.log'"), "{script}");
    assert!(script.contains("> '/opt/my app/app.pid'"), "{script}");
    // 命令是用户写的**代码** → 原样，不能被引号包起来
    assert!(
        script.contains("nohup node server.js --flag=1 >"),
        "{script}"
    );
}

#[test]
fn start_script_neutralizes_an_injected_directory() {
    // 目录名里塞命令 —— 引号之后它必须只是一个**字面量目录名**。
    //
    // ⚠️ 这里**不能**用 `!script.contains("; rm -rf /; echo '")` 来判：
    // 那个串**一定会出现** —— 转义把 `'\''` 里的那个 `'` 刚好插在
    // `; rm -rf /; echo ` 后面。第一版就是这么写的，CI 上
    // `core` 因此红了一条（478 过 / 1 挂）。
    //
    // 有意义的是下面两件事：
    //   ① 引号之后的**整体形式**逐字钉死（注入的那串只是数据）；
    //   ② 除了 `'\''` 这一种转义，里面不许有裸单引号。
    let dir = "/opt/a'; rm -rf /; echo '";
    let script = start_script(dir, "true");

    // ① `shell_quote(dir)` 的完整形态
    let dir_q = "'/opt/a'\\''; rm -rf /; echo '\\'''";
    assert!(script.contains(dir_q), "{script}");

    // ② 数一遍（和 `shell_quote` 那条测试同一个判据）
    let inner = &dir_q[1..dir_q.len() - 1];
    let escaped = inner.matches(r"'\''").count();
    assert_eq!(inner.matches('\'').count(), escaped * 3, "{dir_q}");

    // ③ 命令本身没被目录名吃掉，而且大括号分组还在
    //   （`&` 的优先级最低，少了 `{{ }}` 那个 `echo $! > …` 会跑到
    //    分组外面去 —— 模块文档里那条实测）
    assert!(script.contains("nohup true >"), "{script}");
    assert!(script.ends_with("; }"), "{script}");
}

#[test]
fn status_script_and_its_parser_agree() {
    let script = status_script("/opt/app/app.pid");
    assert!(script.contains(STATUS_RUNNING), "{script}");
    assert!(script.contains(STATUS_STOPPED), "{script}");
    assert!(script.contains("kill -0"), "{script}");
    // ⚠️ 僵尸必须算"没在跑"：`kill -0` 对僵尸也返回 0，而"启动失败"恰好留下
    // 一个僵尸（实机：pidfile 里那个 `[nohup]`，PPID=1，PID 1 不回收子进程）
    // —— 少了这一条，界面会一直显示"运行中"，而应用早就死了。
    assert!(script.contains("/proc/"), "{script}");
    assert!(script.contains("!= Z"), "{script}");
    // 读状态不能用 `awk`：精简镜像里不一定有它（纯 shell 剥 `(comm)` 就够）
    assert!(!script.contains("awk"), "{script}");

    assert_eq!(
        parse_process_status(STATUS_RUNNING),
        Some(ProcessStatus::Running)
    );
    assert_eq!(
        parse_process_status(&format!("{STATUS_STOPPED}\n")),
        Some(ProcessStatus::Stopped)
    );
    // 认不出来是 `None`，**不是**"已停止" —— 两者界面文案不同
    assert_eq!(parse_process_status("error: 别的东西"), None);
    assert_eq!(parse_process_status(""), None);
}

#[test]
fn stop_script_guards_the_missing_pidfile() {
    let script = stop_script("/opt/app/app.pid");
    // 没有 `test -f` 的话 `kill $(cat …)` 会因为**没有参数**报一句用法错误
    assert!(script.contains("if test -f"), "{script}");
    assert!(script.contains("rm -f"), "{script}");
}

#[test]
fn tail_and_count_scripts_use_sentinels() {
    assert!(tail_script("/opt/app/app.log", 200).contains("tail -n 200"));
    assert!(tail_script("/opt/app/app.log", 200).contains(NO_LOG));
    assert!(count_files_script("/opt/app").contains("find"));
    assert!(count_files_script("/opt/app").contains(NO_DIR));
}

#[test]
fn entry_count_only_looks_at_the_top_level() {
    // ⚠️ 必须是 `-mindepth 1 -maxdepth 1`：只数**顶层条目**。
    // 少了 `-maxdepth 1` 就会把整棵树都数进来（那和"复制了几个名字"
    // 对不上），少了 `-mindepth 1` 会把目标目录自己也算一个。
    let script = count_entries_script("/opt/app");
    assert!(script.contains("-mindepth 1 -maxdepth 1"), "{script}");
    assert!(script.contains(NO_DIR), "{script}");
    // 解析用的是同一个函数（`wc -l` 的输出形状一样）
    assert_eq!(parse_file_count("      3\n"), Some(3));
    assert_eq!(parse_file_count(NO_DIR), None);
}

#[test]
fn probe_script_asks_for_the_process_only_in_process_shape() {
    let content = probe_script(DeployKind::Content, "/usr/share/nginx/html");
    assert!(!content.contains(STATUS_RUNNING), "{content}");
    assert!(content.contains(NO_DIR), "{content}");

    let process = probe_script(DeployKind::Process, "/opt/app");
    assert!(process.contains(STATUS_RUNNING), "{process}");
    // pidfile 是从目录推出来的，不需要调用方再传一遍
    assert!(process.contains("/opt/app/app.pid"), "{process}");
    // 顺序固定：进程状态**在前**、文件数在后（解析按位置取）
    let status_at = process.find(STATUS_RUNNING).unwrap();
    let count_at = process.find("find ").unwrap();
    assert!(status_at < count_at, "{process}");
}

#[test]
fn probe_output_is_parsed_by_position_not_by_last_line() {
    // ⚠️ 关键：两行都要各自拿到，不能都取最后一行
    let out = format!("{STATUS_RUNNING}\n      12\n");
    let parsed = parse_probe_output(DeployKind::Process, &out);
    assert_eq!(parsed.process, Some(ProcessStatus::Running));
    assert_eq!(parsed.files, Some(12));

    // 停止 + 目录不在（"部署过但目录没了"）
    let out = format!("{STATUS_STOPPED}\n{NO_DIR}\n");
    let parsed = parse_probe_output(DeployKind::Process, &out);
    assert_eq!(parsed.process, Some(ProcessStatus::Stopped));
    assert_eq!(parsed.files, None, "目录不在是 None，不是 Some(0)");

    // 内容形态没有进程可问
    let parsed = parse_probe_output(DeployKind::Content, "      3\n");
    assert_eq!(parsed.process, None);
    assert_eq!(parsed.files, Some(3));

    let parsed = parse_probe_output(DeployKind::Content, NO_DIR);
    assert_eq!(parsed.files, None);

    // 空输出（不该发生）不能 panic
    let parsed = parse_probe_output(DeployKind::Process, "");
    assert_eq!(parsed.process, None);
    assert_eq!(parsed.files, None);
}

#[test]
fn remove_script_quotes_and_confirms() {
    let members = vec!["index.html".to_owned()];
    let script = remove_script("/usr/share/nginx/html", &members);
    assert_eq!(
        script,
        "if test -d '/usr/share/nginx/html'; then rm -rf \
         '/usr/share/nginx/html/index.html' '/usr/share/nginx/html/app.log' \
         '/usr/share/nginx/html/app.pid'; fi; echo __WSLC_STOPPED__"
    );
    // 目录已经不在了**不算失败**（容器重建过就是这种）
    assert!(script.contains("if test -d"), "{script}");
    // 注入形状必须被引号吃掉
    let injected = remove_script("/opt/a; rm -rf /", &members);
    assert!(injected.contains("'/opt/a; rm -rf /'"), "{injected}");
}

// -----------------------------------------------------------------------
// 目标目录的安全闸
// -----------------------------------------------------------------------

#[test]
fn safe_target_allows_what_the_panel_itself_prefills() {
    // 面板自己预填的那两个必须在白名单里，否则用户"不改就点"会直接被拦
    assert!(
        safe_target(&AppRuntime::Nginx.default_target_dir("site")),
        "nginx 的默认 Web 根必须允许"
    );
    assert!(safe_target(&AppRuntime::Java.default_target_dir("shop")));
    assert!(safe_target("/usr/share/nginx/html"));
    assert!(safe_target("/var/www/site"));
    assert!(safe_target("/srv/web"));
    assert!(safe_target("/opt/app/"));
}

#[test]
fn safe_target_rejects_the_catastrophic_ones() {
    for target in [
        // 根目录 / 一层目录 —— `find <它> -mindepth 1 -delete` 会毁掉容器
        "/",
        "/usr",
        "/etc",
        "/var",
        "/home",
        "/opt",
        "/tmp",
        "/app",
        // `/usr/share` 只差一层，但危险面是"这一层的一切"
        "/usr/share",
        "/usr/lib",
        // 白名单之外
        "/mnt/data",
        "/media/usb",
        "/root/x",
        "/boot/x",
        // 相对路径的含义取决于容器的 WORKDIR，太含糊
        "opt/app",
        "/opt",
        "",
        "   ",
        // 路径穿越
        "/opt/../usr/share",
        "/opt/app/../../etc",
        // 前缀对了但就是前缀本身（`target.len() > prefix.len()` 那条）
        "/opt/",
    ] {
        assert!(!safe_target(target), "{target:?} 必须被拒");
    }
}

#[test]
fn safe_target_also_allows_the_containers_own_site_dir() {
    // 白名单里的照旧（多传一个站点目录不该改变原有结论）
    assert!(safe_target_with_site("/opt/site", Some("/site")));
    assert!(safe_target_with_site("/opt/site", None));

    // ⚠️ 这是这个函数存在的**唯一**理由：用户把站点挂在白名单外，
    // 面板从挂载点推出来的默认值必须能过自己的闸 —— 否则是死胡同。
    assert!(safe_target_with_site("/site", Some("/site")));
    assert!(safe_target_with_site("/site/v2", Some("/site")));
    assert!(safe_target_with_site("/site/", Some("/site/")));
    assert!(safe_target_with_site("/webroot", Some("/webroot")));

    // 站点目录**之外**的仍然一律拒
    assert!(!safe_target_with_site("/etc", Some("/site")));
    assert!(!safe_target_with_site("/usr", Some("/site")));
    // 路径边界：`/sitemap` 不在 `/site` 底下（前缀相同但不是一层）
    assert!(!safe_target_with_site("/sitemap", Some("/site")));
    // 站点目录往上一层也不行（`..` 那条）
    assert!(!safe_target_with_site("/site", Some("/site/../etc")));

    // 站点目录自己就不是个正常路径时，**一行都不多放**
    assert!(!safe_target_with_site("/usr", Some("/")));
    assert!(!safe_target_with_site("/usr", Some("")));
    assert!(!safe_target_with_site("/usr", Some("   ")));
    assert!(!safe_target_with_site("/usr", Some("site")));
    assert!(!safe_target_with_site("/usr", Some("/site/../..")));
}

#[test]
fn safe_target_hint_lists_what_is_allowed() {
    let hint = safe_target_hint();
    assert!(hint.contains("/opt/"), "{hint}");
    // ⚠️ 这里**不再**断言"清空"：v0.9.10 起部署只替换自己铺的文件，
    // 提示里那句话也就跟着改了（断言旧文案会让测试和界面一起撒谎）
    assert!(hint.contains("不能是系统目录"), "要说清为什么被拒：{hint}");
    assert!(hint.len() > 20, "不能是一句空话：{hint}");
}

#[test]
fn log_renderer_translates_the_sentinel() {
    let text = render_log_output(NO_LOG);
    assert!(text.contains("还没有日志文件"), "{text}");
    assert!(!text.contains(NO_LOG), "哨兵不该露给用户：{text}");
    assert_eq!(
        render_log_output(""),
        "（日志是空的 —— 应用启动了，但还没有任何输出）"
    );
    assert_eq!(render_log_output("a\nb\n\n"), "a\nb");
}

#[test]
fn file_count_parser_handles_wc_padding_and_missing_dir() {
    // `wc -l` 的输出左边有空格
    assert_eq!(parse_file_count("      12\n"), Some(12));
    assert_eq!(parse_file_count("0"), Some(0));
    assert_eq!(parse_file_count(NO_DIR), None);
    assert_eq!(parse_file_count("垃圾"), None);
}

// -----------------------------------------------------------------------
// 参数
// -----------------------------------------------------------------------

#[test]
fn exec_args_put_positionals_last() {
    // `wslc` 的位置参数解析遇到第一个非选项就只认位置参数了 ——
    // 开关跑到容器名后面会被吞掉
    let args = exec_args("web", "nginx -s reload");
    assert_eq!(args[0], "exec");
    assert_eq!(args[1], "web");
    assert_eq!(args[2..], ["/bin/sh", "-c", "nginx -s reload"]);
    // 非交互那条**不带 `-i`**：部署里跑的都是"跑完就返回"的命令。
    assert!(!args.contains(&"-i".to_owned()));
}

#[test]
fn artifact_name_takes_the_last_segment_only() {
    assert_eq!(
        artifact_name(Path::new(r"E:\build\myapp.jar")).as_deref(),
        Some("myapp.jar")
    );
    assert_eq!(artifact_name(Path::new("")), None);
}

#[test]
fn container_join_never_doubles_the_slash() {
    assert_eq!(container_join("/opt/app", "app.log"), "/opt/app/app.log");
    assert_eq!(container_join("/opt/app/", "app.log"), "/opt/app/app.log");
    assert_eq!(container_join("/", "app.log"), "/app.log");
    assert_eq!(container_join("", "app.log"), "/app.log");
}

// -----------------------------------------------------------------------
// 默认值
// -----------------------------------------------------------------------

#[test]
fn formats_defaults_per_runtime() {
    assert_eq!(AppRuntime::Nginx.kind(), DeployKind::Content);
    assert_eq!(AppRuntime::Java.kind(), DeployKind::Process);

    assert_eq!(
        AppRuntime::Nginx.default_target_dir("x"),
        "/usr/share/nginx/html"
    );
    assert_eq!(AppRuntime::Java.default_target_dir("shop"), "/opt/shop");

    assert_eq!(AppRuntime::Nginx.default_start_command("x", None), None);
    // Java / Go 要的是**源文件的名字**（见函数文档）
    assert_eq!(
        AppRuntime::Java
            .default_start_command("shop", Some("shop-1.2.jar"))
            .as_deref(),
        Some("java -jar shop-1.2.jar")
    );
    assert_eq!(
        AppRuntime::Java
            .default_start_command("shop", None)
            .as_deref(),
        Some("java -jar shop.jar")
    );
    assert_eq!(
        AppRuntime::Go
            .default_start_command("api", Some("api"))
            .as_deref(),
        Some("./api")
    );
    assert_eq!(
        AppRuntime::Node
            .default_start_command("web", None)
            .as_deref(),
        Some("npm start")
    );

    // 只有 Go 要补可执行位
    assert!(AppRuntime::Go.needs_exec_bit());
    assert!(!AppRuntime::Java.needs_exec_bit());
    assert!(!AppRuntime::Node.needs_exec_bit());
    assert!(!AppRuntime::Nginx.needs_exec_bit());

    // 端口只是信息性的默认值
    assert_eq!(AppRuntime::Nginx.default_port(), 80);
    assert_eq!(AppRuntime::Node.default_port(), 3000);
}

#[test]
fn every_runtime_has_a_label_and_a_note() {
    for runtime in AppRuntime::ALL {
        assert!(!runtime.label().is_empty());
        assert!(!runtime.note().is_empty());
        assert!(!runtime.kind().label().is_empty());
    }
}

#[test]
fn runtime_keys_are_stable_distinct_and_round_trip() {
    // 存档里的键必须是**稳定的小写键**，不是显示名（`Node.js`）也不是
    // Rust 变体名（`Go`）—— 否则手改存档会很难看，改名还会读废老档
    let keys: Vec<&str> = AppRuntime::ALL.iter().map(|r| r.key()).collect();
    assert_eq!(keys, vec!["nginx", "java", "node", "go"]);
    for runtime in AppRuntime::ALL {
        assert_eq!(AppRuntime::from_key(runtime.key()), Some(runtime));
        assert!(runtime.key().chars().all(|c| c.is_ascii_lowercase()));
    }
    // 认不出来的键 → `None`（调用方跳过那条，别猜默认值）
    assert_eq!(AppRuntime::from_key("rust"), None);
    assert_eq!(AppRuntime::from_key(""), None);
    // 前后空格容错
    assert_eq!(AppRuntime::from_key(" go "), Some(AppRuntime::Go));
}

// -----------------------------------------------------------------------
// 错误分类
// -----------------------------------------------------------------------

#[test]
fn classifies_the_texts_we_actually_measured() {
    // 全部是实测抓下来的原文（见模块文档那张表）
    assert_eq!(
        classify_error(
            "容器\"947c4f25…\"未运行。\n错误代码： WSLC_E_CONTAINER_NOT_RUNNING",
            Some(1)
        ),
        DeployErrorKind::ContainerNotRunning
    );
    assert_eq!(
        classify_error("sh: 1: /opt/probe/runme: Permission denied", Some(126)),
        DeployErrorKind::PermissionDenied
    );
    assert_eq!(
        classify_error("sh: 1: tar: not found", Some(127)),
        DeployErrorKind::MissingTool
    );
    assert_eq!(
        classify_error("The system cannot find the path specified.", Some(1)),
        DeployErrorKind::PathNotFound
    );
    assert_eq!(
        classify_error(
            "tar: /nope: Cannot open: No such file or directory",
            Some(2)
        ),
        DeployErrorKind::PathNotFound
    );
    assert_eq!(
        classify_error(
            "OCI runtime exec failed: exec: \"mysql\": executable file not found in $PATH",
            Some(126)
        ),
        DeployErrorKind::MissingTool
    );
    assert_eq!(classify_error("没什么线索", None), DeployErrorKind::Other);
}

/// ⚠️ 回归钉（v0.9.13）：`mkdir` / `cp` 真实报出来的那三种"路径不可用"。
///
/// 三段文本都是**本机跑出来的原文**（wslc 3.0.3.0 + 一个 alpine 容器）：
/// 前两条是把目标目录填成"一个已存在的文件"和"穿过一个文件的路径"
/// （最像手滑的两种），第三条是 `cp` 打到不存在的目录。
///
/// 在补这一条之前，它们**全都落到 `Other`**，而 `Other` 没有 `hint()`
/// —— 用户只拿到引擎原文，尽管"换一个目录试试"就是对的。
#[test]
fn classifies_the_path_errors_we_just_measured() {
    for text in [
        "mkdir: cannot create directory '/tmp/afile': File exists",
        "mkdir: cannot create directory '/tmp/afile': Not a directory",
        "来自引擎的错误信息: Could not find the file /tmp/nope-dir/ in container 453766a51f44\n错误代码： ERROR_PATH_NOT_FOUND",
    ] {
        assert_eq!(
            classify_error(text, Some(1)),
            DeployErrorKind::PathNotFound,
            "{text}"
        );
    }
    // `lower` 是拿整段做的，所以大小写 / 前缀都不影响判断
    assert_eq!(
        classify_error("ERROR_PATH_NOT_FOUND", None),
        DeployErrorKind::PathNotFound
    );
}

#[test]
fn container_not_running_wins_over_everything_else() {
    // 这条报错里同时有中文"未运行"，别被后面的关键词抢走
    let text = "容器\"x\"未运行。错误代码： WSLC_E_CONTAINER_NOT_RUNNING\nPermission denied";
    assert_eq!(
        classify_error(text, Some(1)),
        DeployErrorKind::ContainerNotRunning
    );
}

#[test]
fn every_error_kind_has_a_label_and_an_actionable_hint() {
    for kind in [
        DeployErrorKind::ContainerNotRunning,
        DeployErrorKind::MissingTool,
        DeployErrorKind::PermissionDenied,
        DeployErrorKind::PathNotFound,
    ] {
        assert!(!kind.label().is_empty());
        let hint = kind.hint().expect("这几类都要给下一步");
        assert!(hint.len() > 10, "建议不能是一句空话：{hint}");
    }
    // `Other` 刻意没有建议（就看原文）
    assert_eq!(DeployErrorKind::Other.hint(), None);
}

// -----------------------------------------------------------------------
// 端口发布
// -----------------------------------------------------------------------

#[test]
fn publishing_is_matched_by_container_port() {
    // 本机那个 nginx 容器的实测值
    let ports = vec![(
        "80/tcp".to_owned(),
        vec![("127.0.0.1".to_owned(), "80".to_owned())],
    )];
    assert!(port_is_published(&ports, 80));
    assert!(!port_is_published(&ports, 8080), "没发布的端口要如实说不");
    // ⚠️ `8080` 不能被 `80/tcp` 匹配上（前缀要带那个 `/`）
    assert!(!port_is_published(&ports, 8));
}

#[test]
fn a_port_key_with_no_bindings_counts_as_not_published() {
    // 容器**声明**了端口但没映射到主机时就是这个形状（`"80/tcp": []`）
    let ports = vec![("80/tcp".to_owned(), Vec::new())];
    assert!(
        !port_is_published(&ports, 80),
        "有键但没有实际绑定（HostIp/HostPort 缺失）不算发布出去"
    );
    // 一个映射都没有时不 panic
    assert!(!port_is_published(&[], 80));
}

// -----------------------------------------------------------------------
// 多应用：分配 / 冲突
// -----------------------------------------------------------------------

/// 造一份 `inspect.Ports` 形状的夹具。
fn ports_map(entries: &[(&str, Vec<(&str, &str)>)]) -> PortMap {
    entries
        .iter()
        .map(|(key, binds)| {
            (
                (*key).to_owned(),
                binds
                    .iter()
                    .map(|(host, port)| ((*host).to_owned(), (*port).to_owned()))
                    .collect(),
            )
        })
        .collect()
}

#[test]
fn suggest_app_name_picks_the_first_free_suffix() {
    assert_eq!(suggest_app_name(&[]), "app");
    assert_eq!(suggest_app_name(&["app".to_owned()]), "app-2");
    assert_eq!(
        suggest_app_name(&["app".to_owned(), "app-2".to_owned()]),
        "app-3"
    );
    // 名字里的空白不该让它"看起来不冲突"
    assert_eq!(suggest_app_name(&[" app ".to_owned()]), "app-2");
    assert_eq!(suggest_app_name(&["other".to_owned()]), "app");
}

#[test]
fn suggest_app_port_prefers_published_free_ports() {
    assert_eq!(suggest_app_port(&[], &[80, 8080], 8080), 80);
    assert_eq!(suggest_app_port(&[80], &[80, 8080], 8080), 8080);
    // 已发布的都占了 → 从 default 往上找第一个空的
    assert_eq!(suggest_app_port(&[80, 8080], &[80, 8080], 8080), 8081);
    assert_eq!(suggest_app_port(&[], &[], 3000), 3000);
    assert_eq!(suggest_app_port(&[3000, 3001], &[], 3000), 3002);
    // 0 永远不返回
    assert_eq!(suggest_app_port(&[], &[0], 0), 1);
}

#[test]
fn published_container_ports_only_counts_published_tcp() {
    let ports = ports_map(&[
        ("80/tcp", vec![("127.0.0.1", "8080")]),
        ("8080/tcp", vec![]),
        ("8081/udp", vec![("127.0.0.1", "8081")]),
        ("8082/tcp", vec![("127.0.0.1", "8082")]),
    ]);
    assert_eq!(published_container_ports(&ports), vec![80, 8082]);
}

#[test]
fn app_url_maps_loopback_bindings_to_localhost() {
    let ports = ports_map(&[
        ("80/tcp", vec![("127.0.0.1", "18080")]),
        ("8080/tcp", vec![("0.0.0.0", "18081")]),
        ("8081/tcp", vec![]),
        ("8082/tcp", vec![("::1", "18082")]),
    ]);
    assert_eq!(
        app_url(&ports, 80).as_deref(),
        Some("http://localhost:18080")
    );
    assert_eq!(
        app_url(&ports, 8080).as_deref(),
        Some("http://localhost:18081")
    );
    assert_eq!(app_url(&ports, 8081), None, "空 bindings 不算发布");
    assert_eq!(
        app_url(&ports, 8082).as_deref(),
        Some("http://localhost:18082")
    );
    assert_eq!(app_url(&ports, 9999), None);
}

#[test]
fn default_app_target_puts_nginx_apps_in_their_own_subdir() {
    // ⚠️ 这条是多应用的核心：nginx 的默认目标**不是** Web 根本身。
    assert_eq!(
        default_app_target(AppRuntime::Nginx, "api", Some("/data/www")),
        "/data/www/api"
    );
    assert_eq!(
        default_app_target(AppRuntime::Nginx, "api", Some("/data/www/")),
        "/data/www/api"
    );
    assert_eq!(
        default_app_target(AppRuntime::Nginx, "api", None),
        "/usr/share/nginx/html/api"
    );
    assert_eq!(
        default_app_target(AppRuntime::Java, "api", Some("/data/www")),
        "/opt/api"
    );
    assert_eq!(
        default_app_target(AppRuntime::Node, "api", None),
        "/opt/api"
    );
}

#[test]
fn target_overlap_compares_by_path_boundary() {
    assert_eq!(
        target_overlap("/root/app2", "/root"),
        Some(TargetOverlap::NewInsideExisting)
    );
    assert_eq!(
        target_overlap("/root", "/root/app2"),
        Some(TargetOverlap::ExistingInsideNew)
    );
    assert_eq!(
        target_overlap("/root", "/root/"),
        Some(TargetOverlap::Same),
        "尾部 / 要归一化"
    );
    assert_eq!(
        target_overlap("/root", "/root2"),
        None,
        "按路径边界，不是前缀"
    );
    assert_eq!(
        target_overlap("/", "/root"),
        Some(TargetOverlap::ExistingInsideNew)
    );
    assert_eq!(
        target_overlap("/root", "/"),
        Some(TargetOverlap::NewInsideExisting)
    );
}

/// `first_segment_under` 现在**自己归一化**两个入参（v0.22.3 收口时改的）——
/// 原先调用点必须先把尾部 `/` 去掉再传，漏一次就得到 `None`（"看着能过"）。
#[test]
fn first_segment_under_normalizes_both_sides() {
    assert_eq!(first_segment_under("/root/assets", "/root"), Some("assets"));
    // 同一件事，只是两头带 / 或者带空格
    assert_eq!(
        first_segment_under("/root/assets/", "/root/"),
        Some("assets")
    );
    assert_eq!(
        first_segment_under(" /root/assets ", " /root "),
        Some("assets")
    );
    // 根目录是特例：`/` 底下的第一段
    assert_eq!(first_segment_under("/root", "/"), Some("root"));
    assert_eq!(first_segment_under("/", "/"), None, "根底下没有第一段");
    // 不在底下 / 按边界才算
    assert_eq!(first_segment_under("/root2/assets", "/root"), None);
    assert_eq!(
        first_segment_under("/root", "/root"),
        None,
        "自己是自己但没下一段"
    );
}

// -----------------------------------------------------------------------
// nginx 站点配置
// -----------------------------------------------------------------------

fn site(proxies: Vec<ProxyRule>) -> DeploySite {
    DeploySite {
        server_name: "localhost".to_owned(),
        spa: true,
        proxies,
        conf: String::new(),
    }
}

#[test]
fn site_conf_has_the_measured_shape() {
    let text = site_conf(
        "site",
        &site(vec![]),
        "/usr/share/nginx/html/site",
        8080,
        None,
    );
    assert!(text.contains("listen 8080;"), "{text}");
    assert!(text.contains("server_name localhost;"), "{text}");
    assert!(
        text.contains("root \"/usr/share/nginx/html/site\";"),
        "{text}"
    );
    assert!(text.contains("try_files $uri $uri/ /index.html;"), "{text}");
    assert!(!text.contains("resolver"), "{text}");
    assert!(text.contains("# wslc-panel:site:site"), "{text}");
    let marker = conf_marker("site");
    assert_eq!(
        text.lines().next(),
        Some(marker.as_str()),
        "首行必须恰好是标记（apply/remove 做等值比较）"
    );
}

#[test]
fn site_conf_puts_resolver_inside_server_and_uses_variables() {
    let rule = ProxyRule {
        prefix: "/api/".to_owned(),
        upstream: "http://api:8080".to_owned(),
        strip_prefix: false,
    };
    let text = site_conf(
        "site",
        &site(vec![rule]),
        "/data/www/site",
        8080,
        Some("127.0.0.11"),
    );
    let server_at = text.find("server {").expect("server");
    let resolver_at = text.find("resolver 127.0.0.11").expect("resolver");
    assert!(resolver_at > server_at, "resolver 必须在 server 里：{text}");
    assert_eq!(text.matches("resolver ").count(), 1, "{text}");
    assert!(text.contains("location /api/ {"), "{text}");
    assert!(
        text.contains("proxy_pass $wslc_upstream_1$request_uri;"),
        "{text}"
    );
    assert!(
        !text.contains("proxy_pass http://api:8080"),
        "不能用静态上游：{text}"
    );
}

#[test]
fn site_conf_strip_prefix_uses_a_regex_location() {
    let rule = ProxyRule {
        prefix: "/api/".to_owned(),
        upstream: "http://api:8080".to_owned(),
        strip_prefix: true,
    };
    let text = site_conf(
        "site",
        &site(vec![rule]),
        "/data/www/site",
        8080,
        Some("127.0.0.11"),
    );
    assert!(text.contains("location ~ ^/api/(.*)$ {"), "{text}");
    assert!(
        text.contains("proxy_pass $wslc_upstream_1/$1$is_args$args;"),
        "{text}"
    );
}

#[test]
fn site_conf_quotes_a_root_with_spaces() {
    let text = site_conf("site", &site(vec![]), "/mnt/e/my site", 80, None);
    assert!(text.contains("root \"/mnt/e/my site\";"), "{text}");
}

#[test]
fn upstreams_are_normalized_and_injection_is_rejected() {
    assert_eq!(normalize_upstream("api:8080").unwrap(), "http://api:8080");
    assert_eq!(
        normalize_upstream("http://api:8080").unwrap(),
        "http://api:8080"
    );
    assert_eq!(
        normalize_upstream("https://api:443").unwrap(),
        "https://api:443"
    );
    for bad in [
        "",
        "api",
        "api:0",
        "http://api",
        "http://a:1; } server { listen 9;",
        "http://a:1/x",
    ] {
        assert!(normalize_upstream(bad).is_err(), "应当拒绝：{bad}");
    }
    assert_eq!(upstream_host("http://api:8080"), "api");
}

#[test]
fn prefixes_are_strict() {
    assert_eq!(valid_proxy_prefix(" /api/ ").unwrap(), "/api/");
    for bad in ["/api", "/", "api/", "/a b/", "/a//b/", "/a;b/"] {
        assert!(valid_proxy_prefix(bad).is_err(), "应当拒绝：{bad}");
    }
}

#[test]
fn first_nameserver_takes_the_first_valid_line() {
    let text = "# comment\nsearch lan\nnameserver 127.0.0.11\nnameserver 8.8.8.8\n";
    assert_eq!(first_nameserver(text).as_deref(), Some("127.0.0.11"));
    assert_eq!(first_nameserver("search lan\n"), None);
    assert_eq!(
        first_nameserver("nameserver 2001:db8::1\n").as_deref(),
        Some("2001:db8::1")
    );
}

#[test]
fn nginx_noise_is_filtered_but_real_lines_survive() {
    let text = "nginx: [warn] conflicting server name \"localhost\" on 0.0.0.0:80, ignored\n\
                nginx: [notice] signal process started";
    let clean = filter_nginx_noise(text);
    assert!(!clean.contains("conflicting server name"), "{clean}");
    assert!(clean.contains("signal process started"), "{clean}");
}

#[test]
fn conf_paths_are_ours_and_sorted_first() {
    assert_eq!(
        conf_path("site"),
        "/etc/nginx/conf.d/00-wslc-panel-site.conf"
    );
    assert_eq!(conf_marker("site"), "# wslc-panel:site:site");
}

#[test]
fn common_user_network_ignores_builtin_networks() {
    let left = vec!["bridge".to_owned(), "wslc-panel".to_owned()];
    let right = vec!["bridge".to_owned(), "wslc-panel".to_owned()];
    assert_eq!(
        common_user_network(&left, &right).as_deref(),
        Some(PANEL_NETWORK)
    );
    assert_eq!(common_user_network(&left, &["bridge".to_owned()]), None);
    assert_eq!(common_user_network(&[], &[]), None);
    let left = vec!["host".to_owned(), "none".to_owned(), "app-net".to_owned()];
    let right = vec!["app-net".to_owned(), "host".to_owned()];
    assert_eq!(
        common_user_network(&left, &right).as_deref(),
        Some("app-net")
    );
}

#[test]
fn server_names_are_validated_and_empty_means_localhost() {
    assert_eq!(valid_server_name(" example.com ").unwrap(), "example.com");
    assert_eq!(valid_server_name("").unwrap(), "localhost");
    assert_eq!(valid_server_name("_").unwrap(), "_");
    for bad in ["a b", "a;b", "a{b", "a\"b", "a/b"] {
        assert!(valid_server_name(bad).is_err(), "应当拒绝：{bad}");
    }
}

#[test]
fn proxy_rules_parse_and_format_round_trip() {
    let text = "/api/ => http://api:8080; /auth/ => https://auth:9000 strip";
    let rules = parse_proxy_rules(text).unwrap();
    assert_eq!(rules.len(), 2);
    assert_eq!(rules[0].prefix, "/api/");
    assert_eq!(rules[0].upstream, "http://api:8080");
    assert!(!rules[0].strip_prefix);
    assert_eq!(rules[1].upstream, "https://auth:9000");
    assert!(rules[1].strip_prefix);
    let formatted: Vec<String> = rules.iter().map(format_proxy_rule).collect();
    assert_eq!(parse_proxy_rules(&formatted.join("; ")).unwrap(), rules);
    // 多行 Textarea 里用户会按提示换行；换行与分号必须等价
    assert_eq!(
        parse_proxy_rules("/api/ => http://api:8080\n/auth/ => https://auth:9000 strip").unwrap(),
        rules
    );
    // 空串 / 多余分号 / 空行不算规则
    assert!(parse_proxy_rules("  ;  ").unwrap().is_empty());
    assert!(parse_proxy_rules("\n; \n").unwrap().is_empty());
}

#[test]
fn proxy_rules_reject_garbage() {
    for bad in [
        "api",
        "/api => http://api:8080",
        "/api/ => http://api",
        "/api/ => http://a:1; wat",
        "/api/ => http://a:1 strip wat",
    ] {
        assert!(parse_proxy_rules(bad).is_err(), "应当拒绝：{bad}");
    }
}

#[test]
fn site_conf_emits_one_location_per_rule() {
    let rules = vec![
        ProxyRule {
            prefix: "/api/".to_owned(),
            upstream: "http://api:8080".to_owned(),
            strip_prefix: false,
        },
        ProxyRule {
            prefix: "/auth/".to_owned(),
            upstream: "http://auth:9000".to_owned(),
            strip_prefix: true,
        },
    ];
    let mut site = site(rules);
    site.spa = false;
    let text = site_conf("site", &site, "/data/www/site", 8080, Some("127.0.0.11"));
    assert_eq!(text.matches("    location ").count(), 2, "{text}");
    assert!(text.contains("location /api/ {"), "{text}");
    assert!(text.contains("location ~ ^/auth/(.*)$ {"), "{text}");
    assert!(text.contains("$wslc_upstream_2"), "{text}");
}
