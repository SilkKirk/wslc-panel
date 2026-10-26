//! 动作与提示输入：文案、副作用、二次确认的分派。

use super::super::*;

#[test]
fn danger_actions_all_have_copy() {
    let actions = [
        PendingAction::StopContainer("a".into()),
        PendingAction::StopAllContainers { running: 3 },
        PendingAction::KillContainer("a".into()),
        PendingAction::RemoveContainer("a".into()),
        PendingAction::PruneContainers,
        PendingAction::RemoveImage("img".into()),
        PendingAction::RemoveNetwork("net".into()),
        PendingAction::RemoveVolume("vol".into()),
        PendingAction::PruneVolumes,
    ];
    for action in actions {
        assert!(!action.title().is_empty());
        assert!(!action.body().is_empty());
        assert!(!action.confirm_label().is_empty());
    }
}

#[test]
fn stop_all_body_names_the_blast_radius() {
    // 确认框一开就盖住了工具栏上那行"N 个在运行"，所以**影响面必须写进正文**
    // —— 这是"停一个"和"停全部"在文案上唯一的区别，也是这个字段存在的理由。
    let action = PendingAction::StopAllContainers { running: 4 };
    assert_eq!(action.title(), "停止所有容器");
    assert_eq!(action.confirm_label(), "全部停止");

    let body = action.body();
    assert!(body.contains('4'), "{body}");
    assert!(body.contains("正在运行的容器"), "{body}");
    // 停止 = 丢未保存数据，这句不能省（单容器那条也有同样的说明）
    assert!(body.contains("未保存的数据"), "{body}");
    // 还得说清"已经停着的不受影响"，否则用户会以为已退出的那些也要被动
    assert!(body.contains("不受影响"), "{body}");
}

#[test]
fn stop_all_summary_lists_names_and_truncates() {
    assert_eq!(
        stopped_all_message(&["web".to_owned()]),
        "已停止 1 个容器：web"
    );

    // 5 个是上限：正好 5 个不加"等"，第 6 个开始截断
    let five: Vec<String> = (0..5).map(|i| format!("c{i}")).collect();
    assert_eq!(
        stopped_all_message(&five),
        "已停止 5 个容器：c0、c1、c2、c3、c4"
    );
    let six: Vec<String> = (0..6).map(|i| format!("c{i}")).collect();
    let msg = stopped_all_message(&six);
    assert!(msg.starts_with("已停止 6 个容器："), "{msg}");
    assert!(!msg.contains("c5"), "第 6 个名字不该出现在摘要里：{msg}");
    assert!(msg.ends_with('等'), "{msg}");

    // 一行都没打印时**不能**说"已停止 0 个容器"——那读起来像"成功了，但什么也没发生"
    let none: Vec<String> = Vec::new();
    let empty = stopped_all_message(&none);
    assert!(!empty.contains('0'), "{empty}");
    assert!(empty.contains("没有报告"), "{empty}");
}

#[test]
fn prune_volumes_says_it_only_touches_unused_ones() {
    // 「清理」这个词本身很含糊，用户最怕的是"把我正在用的卷删了"。
    let body = PendingAction::PruneVolumes.body();
    assert!(body.contains("没有被任何容器使用"), "{body}");
    assert!(body.contains("不可撤销"), "{body}");
}

#[test]
fn every_distro_action_has_copy_too() {
    let actions = [
        DistroAction::Terminate("Ubuntu".into()),
        DistroAction::ShutdownAll,
        DistroAction::Unregister {
            name: "Ubuntu".into(),
            vhdx_bytes: Some(19_666_042_880),
        },
        DistroAction::Compact("Ubuntu".into()),
        DistroAction::SetSparse {
            name: "Ubuntu".into(),
            sparse: true,
        },
        DistroAction::SetSparse {
            name: "Ubuntu".into(),
            sparse: false,
        },
    ];
    for action in actions {
        assert!(!action.title().is_empty(), "{action:?}");
        assert!(!action.body().is_empty(), "{action:?}");
        assert!(!action.confirm_label().is_empty(), "{action:?}");
    }
}

#[test]
fn sparse_toggle_says_which_way_it_goes() {
    // 两个方向的文案必须不一样，否则用户分不清点了会开还是关
    let on = DistroAction::SetSparse {
        name: "U".into(),
        sparse: true,
    };
    let off = DistroAction::SetSparse {
        name: "U".into(),
        sparse: false,
    };
    assert_eq!(on.title(), "开启稀疏磁盘");
    assert_eq!(off.title(), "关闭稀疏磁盘");
    assert_eq!(on.confirm_label(), "开启");
    assert_eq!(off.confirm_label(), "关闭");
    assert_ne!(on.body(), off.body());
}

#[test]
fn every_prompt_kind_has_copy() {
    for kind in [
        PromptKind::MoveDistro,
        PromptKind::ResizeDistro,
        PromptKind::SetDefaultUser,
        PromptKind::ExportDistro,
        PromptKind::CreateVolume,
    ] {
        assert!(!kind.title().is_empty(), "{kind:?}");
        assert!(!kind.label().is_empty(), "{kind:?}");
        assert!(!kind.placeholder().is_empty(), "{kind:?}");
        assert!(!kind.note().is_empty(), "{kind:?}");
        assert!(!kind.confirm_label().is_empty(), "{kind:?}");
    }
}

#[test]
fn only_the_two_path_prompts_offer_a_picker() {
    // 「浏览…」只对"要填路径"的那两种有意义。给「创建卷」加一个
    // 会让用户以为该去选一个文件。
    assert!(PromptKind::MoveDistro.pick_target().is_some());
    assert!(PromptKind::ExportDistro.pick_target().is_some());
    assert!(PromptKind::ResizeDistro.pick_target().is_none());
    assert!(PromptKind::SetDefaultUser.pick_target().is_none());
    assert!(PromptKind::CreateVolume.pick_target().is_none());
}

// -- 卷名与 exec 命令的纯逻辑 ------------------------------------------

#[test]
fn volume_name_refuses_anything_that_looks_like_a_flag() {
    // 这条是本地挡下来的**唯一**一类：卷名会变成裸 argv，
    // `--help` 之类会被 wslc 当选项而不是名字。
    assert!(validate_volume_name("webdata").is_ok());
    assert!(validate_volume_name("  webdata  ").is_ok());
    // 名字**中间**有横线是正常的，不该被挡
    assert!(validate_volume_name("web-data_1").is_ok());

    assert!(validate_volume_name("--help").is_err());
    assert!(validate_volume_name("-x").is_err());
    assert!(validate_volume_name("").is_err());
    assert!(validate_volume_name("   ").is_err());
    // 前导空格不能让 `-` 蒙混过去（trim 之后再判）
    assert!(validate_volume_name("  --force").is_err());
}

#[test]
fn exec_argv_goes_through_a_shell() {
    // 锁住这个形状：**不要**改成按空格切分。管道 / 重定向 / `&&`
    // 是用户预期能用的，切开之后它们会静默失效（命令看起来还成功了）。
    assert_eq!(
        exec_argv("ls -la /tmp | head -5"),
        vec!["/bin/sh", "-c", "ls -la /tmp | head -5"]
    );
    // `/bin/sh` 而不是 `/bin/bash`：alpine 这类镜像里没有 bash
    assert_eq!(exec_argv("true")[0], "/bin/sh");
    // 原文一个字都不改地传下去（包括引号）
    assert_eq!(exec_argv("echo 'a b'")[2], "echo 'a b'");
}

// -- 「创建容器」的启动命令那一格 ---------------------------------------

#[test]
fn create_command_leaves_the_image_default_alone_when_empty() {
    // 空 → **一条命令都不给**，镜像自己的 ENTRYPOINT / CMD 才有机会生效。
    // 这条是 mysql / nginx 那类镜像的命门：返回 `["/bin/sh","-c",""]` 同样
    // 算"用户给了命令"，会把 CMD 顶掉 —— 表现是"本来能跑，填了个空反而起不来"。
    assert!(create_command("").is_empty());
    // 只敲空格 / 制表符也算没敲（不 trim 就是 `sh -c " "`，起来就退）
    assert!(create_command("   ").is_empty());
    assert!(create_command("\t\n ").is_empty());
}

#[test]
fn create_command_goes_through_a_shell_and_trims() {
    assert_eq!(
        create_command("  tail -f /dev/null  "),
        vec!["/bin/sh", "-c", "tail -f /dev/null"]
    );
    // `/bin/sh` 而不是 `/bin/bash`：alpine 这类镜像里没有 bash
    assert_eq!(create_command("java -jar /app/a.jar")[0], "/bin/sh");
    // 管道 / 重定向不能静默失效（理由同 `exec_argv`）
    assert_eq!(
        create_command("nginx -g 'daemon off;' | tee /tmp/log")[2],
        "nginx -g 'daemon off;' | tee /tmp/log"
    );
}

#[test]
fn confirm_action_delegates_to_the_right_domain() {
    // 包装层只是转发，不能把两边的文案搞混
    let container = ConfirmAction::Container(PendingAction::RemoveVolume("v".into()));
    assert_eq!(container.title(), "删除卷");
    assert_eq!(container.confirm_label(), "删除卷");

    let distro = ConfirmAction::Distro(DistroAction::Terminate("Ubuntu".into()));
    assert_eq!(distro.title(), "终止发行版");
    assert_eq!(distro.confirm_label(), "终止");
    assert!(distro.body().contains("Ubuntu"));
}

#[test]
fn unregister_body_shows_how_much_is_about_to_be_deleted() {
    // 用户在点"删除"之前必须知道自己要删掉多少东西
    let with_size = DistroAction::Unregister {
        name: "Ubuntu".into(),
        vhdx_bytes: Some(19_666_042_880),
    };
    let body = with_size.body();
    assert!(body.contains("18.32 GB"), "{body}");
    assert!(body.contains("不可撤销"), "{body}");

    // 读不到大小时也要能正常出文案（只是没有那句括号）
    let without_size = DistroAction::Unregister {
        name: "Ubuntu".into(),
        vhdx_bytes: None,
    };
    let body = without_size.body();
    assert!(!body.contains("虚拟磁盘约"), "{body}");
    assert!(body.contains("Ubuntu"), "{body}");
}

#[test]
fn prune_warns_about_being_irreversible() {
    assert!(PendingAction::PruneContainers.body().contains("不可撤销"));
    assert!(
        PendingAction::RemoveVolume("v".into())
            .body()
            .contains("不可撤销")
    );
}
