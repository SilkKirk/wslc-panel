//! 快照派生的数字：总览、镜像体积合计、存储环。

use super::super::prelude::*;
use super::super::*;

#[test]
fn snapshot_default_is_empty_and_has_no_data() {
    let snap = Snapshot::default();
    assert!(!snap.has_data());
    assert!(snap.errors.is_empty());
    // 空快照的圆环：全是 0 / None —— 界面上那一排会画成"没有数据"，
    // **不是** 0% 满环。
    let stats = snap.dashboard_stats();
    assert_eq!(stats.running, 0);
    assert_eq!(stats.total, 0);
    assert_eq!(stats.cpu_percent, 0.0);
    assert_eq!(stats.cpu_cores, None);
    assert_eq!(stats.mem_limit, None);
    assert_eq!(stats.cpu_whole_machine_percent(), None);
    assert_eq!(stats.mem_percent(), None);
    // 「容器」那一环：一个容器都没有时 `None`（环空着），不是 0%。
    // 0% 看起来像"一个都没在跑"，而真实情况是"还没有容器"。
    assert_eq!(stats.container_percent(), None);
    assert!(stats.disk.is_none());
}

/// 「容器」那一环的比例（四个环里原先唯一没有测试的那个）。
///
/// 它原先写在 `views/dashboard.rs` 里手算，而 `views/` 不在任何测试目标里
/// （`cargo test --workspace --exclude wslc-panel`）。这条测试就是把它
/// 搬进 `wslc-panel-core` 的理由：口径写错了要能被 CI 拦住。
#[test]
fn container_percent_is_running_over_total() {
    fn stats(running: usize, total: usize) -> DashboardStats {
        DashboardStats {
            running,
            total,
            ..Default::default()
        }
    }

    assert_eq!(stats(0, 4).container_percent(), Some(0.0), "一个都没在跑");
    assert_eq!(stats(4, 4).container_percent(), Some(100.0), "全在跑");
    assert_eq!(stats(1, 4).container_percent(), Some(25.0));
    // ⚠️ 分母是 `total` 不是 `running`：写成 `running / running` 也能算出
    // 100%，但 1/4 会被算成 100% —— 这条边界是它和那个 bug 的分水岭。
    assert_eq!(stats(1, 3).container_percent(), Some(100.0 / 3.0));

    // 一个容器都没有 → `None`（环空着），**不是** `Some(0.0)`
    assert_eq!(stats(0, 0).container_percent(), None);
    // 口径异常（理论上不会出现）也不该 panic、不该算出 >100%
    assert_eq!(stats(8, 4).container_percent(), Some(100.0), "夹到 100");
}

#[test]
fn image_totals_dedupe_refs_pointing_at_the_same_id() {
    // 真机夹具：`e2ac70e7319a` 同时是
    // `docker.1panel.live/library/hello-world` 和 `hello-world` 两行。
    const IMAGES: &str = include_str!("../../../../wslc-core/tests/fixtures/images.jsonl");
    let images: Vec<ImageListItem> = wslc_core::jsonl::parse_lines(IMAGES).expect("夹具应当能解析");
    assert_eq!(images.len(), 3, "夹具本身是 3 行（2 个镜像）");

    let snap = Snapshot {
        images,
        ..Default::default()
    };
    let (distinct, bytes) = snap.image_totals();
    assert_eq!(distinct, 2, "同一个 ID 的多条引用只算一个镜像");
    // 8.42MB + 10.1kB（1000 进制，见 `parse_size`）。浮点转 u64 允许 1 字节误差。
    assert!(
        (bytes as i64 - 8_430_100).abs() <= 1,
        "合计体积应当是 8.42MB + 10.1kB，实际 {bytes}"
    );
}

#[test]
fn image_totals_of_empty_snapshot_is_zero() {
    assert_eq!(Snapshot::default().image_totals(), (0, 0));
}

/// 一个只有 CPU / 内存指标有意义的样本（字段形状照 `wslc stats` 真机输出）。
fn summary_with_stats(cpu: &str, mem: &str) -> ContainerSummary {
    ContainerSummary {
        item: wslc_core::model::ContainerListItem::default(),
        stats: Some(wslc_core::model::ContainerStats {
            cpu_perc: cpu.to_owned(),
            mem_usage: mem.to_owned(),
            ..Default::default()
        }),
    }
}

#[test]
fn dashboard_stats_sums_the_real_machine_numbers() {
    // 真机那一组（`wslc stats` 实测）：redis 0.13% / mysql 0.56%，
    // 内存 27.45MiB + 514.1MiB，上限都是同一个虚拟机的 15.48GiB。
    let mut snap = Snapshot {
        running: vec![
            summary_with_stats("0.13%", "27.45MiB / 15.48GiB"),
            summary_with_stats("0.56%", "514.1MiB / 15.48GiB"),
        ],
        // 全部容器比运行中的多两个（已停止的）
        all: vec![
            summary_with_stats("0.13%", "27.45MiB / 15.48GiB"),
            summary_with_stats("0.56%", "514.1MiB / 15.48GiB"),
            summary_with_stats("0.00%", "3.9MiB / 15.48GiB"),
            summary_with_stats("0.00%", "35.36MiB / 15.48GiB"),
        ],
        wsl_cpu_cores: Some(32),
        ..Default::default()
    };
    // 会话盘：1000 字节里用了 600（VHD 占 120）
    let mut storage = wslc_core::storage::layout(None, PathBuf::from(r"C:\x"), None);
    storage.volume = Some(VolumeSpace {
        total: 1000,
        free: 400,
    });
    storage.vhd_bytes = Some(120);
    snap.storage = Some(storage);

    let stats = snap.dashboard_stats();
    assert_eq!(stats.running, 2);
    assert_eq!(stats.total, 4);
    assert!(
        (stats.cpu_percent - 0.69).abs() < 1e-9,
        "{}",
        stats.cpu_percent
    );
    // 内存是**字节**：MiB 是 1024 进制
    let mib = 1024.0 * 1024.0;
    assert_eq!(stats.mem_used, (27.45 * mib) as u64 + (514.1 * mib) as u64);
    // 上限取容器报的那个（同一个虚拟机，各容器一致）
    assert_eq!(stats.mem_limit, Some((15.48 * 1024.0 * mib) as u64));

    // ⚠️ 这条是这一整块存在的理由：CPU 合计 **不是**整机占用率。
    // `wslc stats` 用的是 Docker 那条公式，100% ≈ 1 个核 ——
    // 0.69% ÷ 32 核 ≈ 0.022%，而不是 0.69%。
    let whole = stats
        .cpu_whole_machine_percent()
        .expect("给了核数就该算得出来");
    assert!((whole - 0.69 / 32.0).abs() < 1e-9, "{whole}");
    // 环下面那行小字（`0.0069 / 32 核`）
    assert!((stats.cpu_cores_used() - 0.0069).abs() < 1e-9);

    // 内存那一环：用了多少 / 上限
    let mem_percent = stats.mem_percent().expect("有上限就该算得出来");
    assert!((mem_percent - (541.55 / (15.48 * 1024.0) * 100.0)).abs() < 1e-6);

    // 存储那一环：**会话 VHD** ÷ 所在盘总量 = 120 / 1000 = 12%
    //
    // ⚠️ 刻意**不是**"盘已用 / 盘总量"（600/1000 = 60%）：那是这台电脑的
    // 磁盘占用率，和面板管的这一摊没关系 —— 实机反馈就是这个
    // （环里写着 50%，而容器数据只有 2.64 GB）。
    let disk = stats.disk.expect("给了 storage 就该有盘");
    assert_eq!(disk.total, 1000);
    assert_eq!(stats.vhd_bytes, Some(120));
    let vhd_percent = stats
        .vhd_percent_of_disk()
        .expect("有 VHD、有盘总量就算得出来");
    assert!((vhd_percent - 12.0).abs() < 1e-9, "{vhd_percent}");
}

#[test]
// ⚠️ `field_reassign_with_default`：这个测试要造**四种**不同的
// `storage` 组合（无 VHD / 无卷 / 正常 / 卷容量 0），而 `Snapshot` 有
// 11 个字段。用 `Snapshot { storage: …, ..Default::default() }` 的话
// 每处都得重复一遍结构体名，反而把"这个测试只关心 storage"埋掉了。
// 所以这里保留 `let mut snap = Snapshot::default(); snap.storage = …`。
#[allow(clippy::field_reassign_with_default)]
fn dashboard_stats_storage_ring_needs_both_the_vhd_and_the_disk() {
    // 缺哪一半都不出百分比：这是**一个比例**，只有分子或只有分母都是编数字。
    // VHD 还没建出来（新装的会话）
    let mut snap = Snapshot::default();
    snap.storage = Some(wslc_core::storage::layout(
        None,
        PathBuf::from(r"C:\x"),
        None,
    ));
    assert_eq!(snap.dashboard_stats().vhd_percent_of_disk(), None);

    // 有 VHD，但读不到卷容量
    let mut snap = Snapshot::default();
    let mut storage = wslc_core::storage::layout(None, PathBuf::from(r"C:\x"), None);
    storage.vhd_bytes = Some(120);
    snap.storage = Some(storage);
    assert_eq!(snap.dashboard_stats().vhd_percent_of_disk(), None);

    // 两个都有 → 算得出来
    let mut storage = wslc_core::storage::layout(None, PathBuf::from(r"C:\x"), None);
    storage.vhd_bytes = Some(120);
    storage.volume = Some(VolumeSpace {
        total: 1000,
        free: 400,
    });
    let mut snap = Snapshot::default();
    snap.storage = Some(storage);
    assert_eq!(snap.dashboard_stats().vhd_percent_of_disk(), Some(12.0));

    // 卷容量是 0（不该发生）→ 不当分母，别去算除零
    let mut storage = wslc_core::storage::layout(None, PathBuf::from(r"C:\x"), None);
    storage.vhd_bytes = Some(120);
    storage.volume = Some(VolumeSpace { total: 0, free: 0 });
    let mut snap = Snapshot::default();
    snap.storage = Some(storage);
    assert_eq!(snap.dashboard_stats().vhd_percent_of_disk(), None);
}

#[test]
fn dashboard_stats_without_cores_reports_no_percentage() {
    // 拿不到核数（`.wslconfig` 没写、环境变量也没读到）时**不能**把
    // 原始合计当百分比：多核机器上 400% 是四个核跑满，画成 400% 的环
    // 只会误导。这时候宁可只给原始合计。
    let snap = Snapshot {
        running: vec![summary_with_stats("400.00%", "1MiB / 2MiB")],
        wsl_cpu_cores: None,
        ..Default::default()
    };
    let stats = snap.dashboard_stats();
    assert_eq!(stats.cpu_percent, 400.0);
    assert_eq!(stats.cpu_whole_machine_percent(), None);
    // 核数写 0（不该发生）也当拿不到，别去算除零
    let zero = Snapshot {
        running: vec![summary_with_stats("10.00%", "")],
        wsl_cpu_cores: Some(0),
        ..Default::default()
    };
    assert_eq!(zero.dashboard_stats().cpu_whole_machine_percent(), None);
}

#[test]
fn dashboard_stats_skips_containers_without_stats_and_nan() {
    // "这一轮没读到统计"和"占用 0"是两件事：前者**不计入**合计
    // （把它当 0 会让合计数看起来比真实占用小 —— 那是编数字）。
    let mut snap = Snapshot {
        running: vec![
            summary_with_stats("1.00%", "1MiB / 4MiB"),
            // 没有 stats：只有列表项
            ContainerSummary {
                item: wslc_core::model::ContainerListItem::default(),
                stats: None,
            },
            // `parse_percent` 对 `nan%` 会给回 NaN —— 不能让它污染合计
            summary_with_stats("nan%", "1MiB / 4MiB"),
        ],
        wsl_cpu_cores: Some(4),
        ..Default::default()
    };
    snap.all = snap.running.clone();

    let stats = snap.dashboard_stats();
    assert_eq!(
        stats.running, 3,
        "运行中数的是**列表**，不是有 stats 的那几个"
    );
    assert_eq!(stats.cpu_percent, 1.0, "NaN 与缺失都不该进合计");
    assert_eq!(stats.mem_used, (1.0 * 1024.0 * 1024.0) as u64 * 2);
}

#[test]
fn dashboard_stats_without_a_memory_limit_has_no_percentage() {
    // 上限是 `0B`（`wslc` 在容器刚起来时会这么报）→ 当没有，别去除零
    let snap = Snapshot {
        running: vec![summary_with_stats("1.00%", "0B / 0B")],
        wsl_cpu_cores: Some(4),
        ..Default::default()
    };
    let stats = snap.dashboard_stats();
    assert_eq!(stats.mem_limit, None);
    assert_eq!(stats.mem_percent(), None);
}
