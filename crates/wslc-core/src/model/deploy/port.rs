//! 端口映射（`PortMap`）的查询。

/// `inspect.Ports` 的形状：`"80/tcp" -> [(host_ip, host_port)]`。
///
/// 单独写一个别名，是因为这个形状出现在两处**结构体签名**里
/// （`cmd::deploy::DeployProbe::ports` 与 `wslc_panel_core::state::DeployState::ports`）
/// —— 元组套元组写两遍必然会有人抄错一层。判据函数
/// [`port_is_published`] 收的是它的**切片**形态（这样测试里传
/// `&[…]` / `&vec![…]` 都能用）。
pub type PortMap = Vec<(String, Vec<(String, String)>)>;

/// 这个**容器端口**有没有发布到主机。
///
/// # 为什么要抽成纯函数（v0.9.13）
///
/// 早先这个判据有两份：`cmd::deploy::DeployProbe::publishes()`（有单测、**没有
/// 生产调用点**）和 `views::deploy_port_cell` 里内联复制的一段。也就是说
/// **跑在界面上的那份从来没被测过** —— 正是本仓库最忌讳的"测的不是跑的那份"。
/// 现在两边都调这里，测试也就真的盯着在跑的那份。
///
/// ⚠️ 匹配要**带那个 `/`**：容器端口的键是 `"80/tcp"` 这种形状，只用
/// `starts_with("80")` 会把 `8080/tcp` 也算成"80 已发布"。
///
/// ⚠️ 有键但**没有实际绑定**（`"80/tcp": []`）不算发布出去 —— `inspect` 在
/// 容器**声明**了端口却没映射到主机时就是这个形状（本机那个 nginx 容器
/// 的 `Labels` 里也能看到同样的空数组）。
pub fn port_is_published(ports: &[(String, Vec<(String, String)>)], port: u16) -> bool {
    let prefix = format!("{port}/");
    ports
        .iter()
        .any(|(key, bindings)| key.starts_with(&prefix) && !bindings.is_empty())
}
