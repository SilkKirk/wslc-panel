# wslc CLI 输出字段字典（实测采集）

> 采集环境：WSL **3.0.1.0**，内核 6.18.40.1-1，Windows 10.0.26300.9550
> 采集方式：真实执行 `wslc` 命令并抓取 stdout（探针容器 `wslc-panel-probe` 已删除，系统恢复 0 容器）
> 原始样本：`crates/wslc-core/tests/fixtures/`

---

## 0. 三条铁律（踩过的坑）

### 0.1 必须注入 `WSL_UTF8=1`

`wslc` 默认输出 **UTF-16LE**，直接按 UTF-8 读会得到乱码或带 `\0` 的字符串。

```powershell
# 不设：字节为 UTF-16LE
cmd /c "wslc info"                         # → UTF-16LE
# 设了：字节为 UTF-8
cmd /c "set WSL_UTF8=1&& wslc info"        # → E5 AE A2 = "客"
```

**实现要求**：`wslc-core::cli` 必须在每个子进程上 `.env("WSL_UTF8", "1")`，
并且 `decode.rs` 仍需保留 UTF-16LE 兜底（BOM `FF FE` / 大量 `00` 交错）。

### 0.2 `--format json` 是 **JSON Lines**

不是 JSON 数组，是**每行一个独立对象**，且末尾有换行：

```jsonl
{"CreatedAt":"...","Driver":"bridge","ID":"93e17bf0fae9",...}
{"CreatedAt":"...","Driver":"host","ID":"c691a0a323d3",...}
```

**例外**：`wslc inspect` 输出的是**标准 JSON 数组**（带缩进），`-f` 可压成单行。

**空结果**：0 条记录时输出**空字符串**（0 字节），不是 `[]`。

### 0.3 字段类型不统一

| 现象 | 例子 |
|---|---|
| 数值被序列化成字符串 | `"Size":"8.42MB"`、`"IPv4":"true"`、`"LocalVolumes":"0"` |
| 数值保持数字 | `"PIDs":1` |
| 布尔保持布尔 | `"Running":true` |
| 可空字段 | `"Entrypoint":null`、`"Health":null` |
| 值里嵌 JSON 字符串 | `Labels` 字段 |

**实现要求**：模型层对可空/可缺字段全部 `#[serde(default)]` + `Option<T>`，
解析失败**不得 panic**，未知字段透传到"原始 JSON"面板。

---

## 1. `wslc info --format json`

单个 JSON 对象（不是 JSONL）：

```json
{
  "Client": {
    "Direct3DVersion": "1.611.1-81528511",
    "DxCoreVersion": "10.0.26100.1-240331-1435.ge-release",
    "KernelVersion": "6.18.40.1-1",
    "SettingsFile": "C:\\Users\\76434\\AppData\\Local\\wslc\\settings.yaml",
    "Version": "3.0.1.0",
    "WindowsVersion": "10.0.26300.9550"
  },
  "Server": {
    "SessionManagerVersion": "3.0.1",
    "Sessions": [
      { "CreatorPid": 21684, "ID": 1, "Name": "wslc-cli-76434" }
    ]
  }
}
```

| 字段 | 类型 | 说明 |
|---|---|---|
| `Client.Version` | string | WSL 版本 |
| `Client.KernelVersion` | string | 内核版本 |
| `Client.Direct3DVersion` | string | Direct3D |
| `Client.DxCoreVersion` | string | DXCore |
| `Client.WindowsVersion` | string | Windows 版本 |
| `Client.SettingsFile` | string | settings.yaml 绝对路径 |
| `Server.SessionManagerVersion` | string | 会话管理器版本 |
| `Server.Sessions[].ID` | number | 会话 ID（对应 `--session`） |
| `Server.Sessions[].Name` | string | 会话显示名 |
| `Server.Sessions[].CreatorPid` | number | 创建者进程 PID |

---

## 2. `wslc list -a --format json` （容器）

**JSON Lines**，每行一个容器。

运行中：

```json
{"Command":"\"sleep 300\"","CreatedAt":"2026-10-08 16:34:46 +0800 GMT+8","HealthStatus":"",
 "ID":"ff0667ee90fb","Image":"docker.1ms.run/library/alpine:latest",
 "Labels":"com.microsoft.wsl.container.metadata={\"V1\":{...}}","LocalVolumes":"0","Mounts":"",
 "Names":"wslc-panel-probe","Networks":"bridge","Platform":{"architecture":"amd64","os":"linux"},
 "Ports":"127.0.0.1:18080->80/tcp","RunningFor":"3 seconds ago","Size":"0B",
 "State":"running","Status":"Up 3 seconds"}
```

已退出（同一容器，`stop` 之后）：

```json
{"...":"...","Ports":"","RunningFor":"39 seconds ago","Size":"0B",
 "State":"exited","Status":"Exited (137) 2 seconds ago"}
```

| 字段 | 类型 | 可空 | 说明 |
|---|---|---|---|
| `ID` | string | | 默认 12 位短 ID；`--no-trunc` 时 64 位全 ID |
| `Names` | string | | 容器名（**无前导 `/`**） |
| `Image` | string | | 镜像引用 |
| `Command` | string | | 命令，**含字面双引号**：`"\"sleep 300\""` |
| `CreatedAt` | string | | `YYYY-MM-DD HH:MM:SS +0800 GMT+8` |
| `RunningFor` | string | | 相对时间：`3 seconds ago` |
| `State` | string | | `running` / `exited` / `created` / `paused` … |
| `Status` | string | | 人类可读：`Up 3 seconds` / `Exited (137) 2 seconds ago` |
| `HealthStatus` | string | ✅ 空串 | 健康检查状态 |
| `Ports` | string | ✅ 空串 | `127.0.0.1:18080->80/tcp`，多段逗号分隔 |
| `Networks` | string | | 网络名 |
| `Mounts` | string | ✅ 空串 | 挂载 |
| `LocalVolumes` | string | | 数字字符串 `"0"` |
| `Size` | string | | `0B`、`8.42MB` |
| `Labels` | string | | 逗号分隔 `k=v`；**值里可能是嵌套 JSON** |
| `Platform` | object | ✅ | `{"architecture":"amd64","os":"linux"}` |

### 2.1 `Labels` 里藏着的端口元数据

```json
com.microsoft.wsl.container.metadata={
  "V1": {
    "Flags": 0, "InitProcessFlags": 0,
    "Ports": [{"BindingAddress":"127.0.0.1","ContainerPort":80,"Family":2,
               "HostPort":18080,"Protocol":6,"VmPort":20002}],
    "Volumes": []
  }
}
```

> `Protocol: 6` = TCP，`17` = UDP。`VmPort` 是 WSL 虚拟机内的转发端口。
> 需要精确端口映射时，优先解析这里而不是 `Ports` 字符串。

---

## 3. `wslc stats --format json`

**JSON Lines**。默认只列运行中；`-a` 含全部。

运行中：

```json
{"BlockIO":"1.51MB / 0B","CPUPerc":"0.00%","ID":"ff0667ee90fb...(64位)",
 "MemPerc":"0.02%","MemUsage":"3.465MiB / 15.48GiB","Name":"wslc-panel-probe",
 "NetIO":"1.04kB / 0B","PIDs":1}
```

已退出（`-a`）：

```json
{"BlockIO":"0B / 0B","CPUPerc":"0.00%","ID":"...","MemPerc":"0.00%",
 "MemUsage":"0B / 0B","Name":"wslc-panel-probe","NetIO":"0B / 0B","PIDs":0}
```

| 字段 | 类型 | 格式 |
|---|---|---|
| `ID` | string | **64 位全 ID**（与 list 默认短 ID 不一致，需前缀匹配） |
| `Name` | string | 容器名 |
| `CPUPerc` | string | `0.00%` |
| `MemPerc` | string | `0.02%` |
| `MemUsage` | string | `已用 / 上限`，如 `3.465MiB / 15.48GiB` |
| `NetIO` | string | `收 / 发` |
| `BlockIO` | string | `读 / 写` |
| `PIDs` | **number** | 数字，不是字符串 |

> 注意 `ID` 长度不一致：**统计与列表必须按前缀匹配，不能全等比较**。

---

## 4. `wslc inspect <name|id>` / `wslc inspect -s`

**标准 JSON 数组**（带缩进）。`-s` 追加 `SizeRootFs` / `SizeRw`。

```json
[
  {
    "Config": {
      "Cmd": ["sleep", "300"],
      "Entrypoint": null,
      "Env": ["PROBE=1", "PATH=/usr/local/sbin:..."],
      "Healthcheck": null,
      "Image": "docker.1ms.run/library/alpine:latest",
      "Labels": {},
      "StopTimeout": null,
      "User": "",
      "WorkingDir": "/"
    },
    "Created": "2026-10-08T08:34:46.367784685Z",
    "HostConfig": { "Memory": 0, "NanoCpus": 0, "NetworkMode": "bridge", "Ulimits": [] },
    "Id": "ff0667ee90fbc7c73f75ab9bf4fc642b924a6e6bdf0ab6e11dfc647e7dddada0",
    "Image": "sha256:320994c3b997...",
    "Labels": {},
    "Mounts": [],
    "Name": "/wslc-panel-probe",
    "NetworkSettings": {
      "Networks": {
        "bridge": { "Aliases": [], "DriverOpts": {}, "Gateway": "172.17.0.1",
                    "IPAMConfig": null, "IPAddress": "172.17.0.2", "IPPrefixLen": 16,
                    "Links": [], "MacAddress": "02:42:ac:11:00:02" }
      }
    },
    "Ports": { "80/tcp": [ { "HostIp": "127.0.0.1", "HostPort": "18080" } ] },
    "SizeRootFs": 8422040,
    "SizeRw": 0,
    "State": { "ExitCode": 0, "FinishedAt": "0001-01-01T00:00:00Z", "Health": null,
               "Running": true, "StartedAt": "2026-10-08T08:34:46.556762492Z",
               "Status": "running" }
  }
]
```

要点：

- `Name` **带前导 `/`**（`/wslc-panel-probe`），与 `list` 的 `Names` 不同 → 需要规范化。
- `Id` 是 64 位；`Image` 是 `sha256:` 摘要，与 `Config.Image`（引用名）不同。
- `Ports` 是 **map**：键 `"80/tcp"`，值是数组。
- 未启动容器的 `FinishedAt` 是零值 `0001-01-01T00:00:00Z`，**不能直接解析成有意义时间**，要判零。
- `Created` / `StartedAt` 是 RFC3339 纳秒精度。
- `HostConfig.Memory` / `NanoCpus` 为 `0` 表示未限制。

---

## 5. `wslc images --format json` （镜像）

**JSON Lines**：

```json
{"Containers":"0","CreatedAt":"2026-09-18 04:37:20 +0800 GMT+8","CreatedSince":"2 weeks ago",
 "Digest":"<none>","ID":"320994c3b997","Repository":"docker.1ms.run/library/alpine",
 "SharedSize":"N/A","Size":"8.42MB","Tag":"latest","UniqueSize":"N/A"}
```

| 字段 | 类型 | 说明 |
|---|---|---|
| `ID` | string | 12 位 |
| `Repository` | string | 仓库，可为 `<none>` |
| `Tag` | string | 标签，可为 `<none>` |
| `Digest` | string | `<none>` 或 `sha256:...` |
| `CreatedAt` / `CreatedSince` | string | 绝对 / 相对时间 |
| `Size` | string | `8.42MB` |
| `SharedSize` / `UniqueSize` | string | 常为 `N/A` |
| `Containers` | string | 数字字符串 |

> **同一镜像会出现多行**（不同 `Repository` 引用指向同一 `ID`），
> 例如 `docker.1panel.live/library/hello-world` 与 `hello-world` 都是 `e2ac70e7319a`。
> UI 需要按 `ID` 聚合或用 `Repository:Tag` 作为主键。

---

## 6. `wslc network list --format json`

**JSON Lines**，全部字段都是字符串：

```json
{"CreatedAt":"2026-10-08 08:34:06.780377919 +0000 UTC","Driver":"bridge","ID":"93e17bf0fae9",
 "IPv4":"true","IPv6":"false","Internal":"false","Labels":"","Name":"bridge","Scope":"local"}
```

内置网络：`bridge` / `host`（driver `host`）/ `none`（driver `null`）。

---

## 7. `wslc volume list --format json`

**JSON Lines**；无卷时输出 0 字节。表头为 `DRIVER  VOLUME NAME`。

---

## 8. `wslc system session list`

**不支持 `--format`**（会报 `当前命令的选项名称未被识别：'--format'`）。只有表格输出：

```
ID   创建者 PID   显示名称
1    21684     wslc-cli-76434
```

可用选项：`--verbose`。

> **实现要求**：会话列表必须走**表格解析**，且表头是**中文**、列宽按字符对齐。
> 需要按表头关键字定位列，而不是硬编码列偏移。

---

## 9. settings.yaml

路径来自 `wslc info` 的 `Client.SettingsFile`，默认
`%LOCALAPPDATA%\wslc\settings.yaml`。`wslc settings` 会用默认编辑器打开它。

```yaml
# wslc user settings
# https://aka.ms/wslc-settings
# All settings support string value "default" which uses built-in defaults.

session:
  # Number of virtual CPUs allocated to the session (e.g. 4 default: all available CPUs)
  # cpuCount: default

  # Memory limit for the session (e.g. 2GB default: half of available memory)
  # memorySize: default

  # Maximum disk image size (e.g. 500GB default: 1TB)
  # maxStorageSize: default

  # Base directory for the default session's storage; the session VHD is created at
  # <storagePath>\wslc\sessions\<session>\storage.vhdx. Must be an absolute path
  # (e.g. D:\data default: %LOCALAPPDATA%). Changing this after a session already exists
  # does not move existing storage, containers, or images; the previous location is left
  # in place and a new empty session is created at the new path.
  # storagePath: default

  # Default host address that published ports bind to when 'container run -p' is
  # used without an explicit address (default: 127.0.0.1)
  # defaultBindingAddress: default

  # DNS name that resolves to the host loopback address (default: host.wslc.internal).
  # Set to "none" to disable the entry.
  # hostLoopback: default

  # Seconds an idle session VM stays running before it is torn down (default: 30)
  # idleTimeout: default

# Credential storage backend: "wincred" or "file" (default: wincred)
# credentialStore: wincred
```

| 键 | 类型 | 默认 | 说明 |
|---|---|---|---|
| `session.cpuCount` | int \| `"default"` | 全部可用 CPU | 分配给会话的 vCPU 数 |
| `session.memorySize` | size \| `"default"` | 物理内存一半 | 会话内存上限，如 `2GB` |
| `session.maxStorageSize` | size \| `"default"` | 1TB | 磁盘镜像上限 |
| `session.storagePath` | 绝对路径 \| `"default"` | `%LOCALAPPDATA%` | VHDX 落盘基目录 |
| `session.defaultBindingAddress` | ip \| `"default"` | `127.0.0.1` | `-p` 未指定地址时的绑定地址 |
| `session.hostLoopback` | DNS 名 \| `"none"` \| `"default"` | `host.wslc.internal` | 解析到宿主 loopback 的 DNS 名 |
| `session.idleTimeout` | 秒 \| `"default"` | `30` | 空闲会话 VM 回收秒数 |
| `credentialStore` | `wincred` \| `file` | `wincred` | 凭据后端（YAML 顶层，不在 `session` 下） |

> ⚠️ 改 `storagePath` **不会迁移**已有容器/镜像，会在新路径新建空会话。
> UI 必须对这一项做显式警告。

**默认 settings.yaml 里所有键都是注释掉的** —— 意味着"默认值"需要应用内置，
且写回时如果要保留注释，不能简单 `serde_yaml` 整体序列化。

---

## 10. 命令清单（用于 UI 能力矩阵）

| 分类 | 命令 |
|---|---|
| 系统 | `info` `version` `settings` `system info` `system events` `system session {list,enter,run,shell,terminate}` |
| 容器 | `list` `create` `run` `start` `stop` `restart` `kill` `remove` `prune` `inspect` `stats` `logs` `exec` `attach` `cp` `export` `events` |
| 镜像 | `images` `pull` `push` `rmi` `tag` `save` `load` `import` `build` `login` `logout` |
| 网络 | `network {list,inspect,create,remove,prune,connect,disconnect}` |
| 卷 | `volume {list,inspect,create,remove,prune}` |
| 凭据 | `registry` |

全局选项：`--session <id>` —— **所有命令都可用**，UI 需要会话切换器。

`--format json` 支持情况（实测）：

| 命令 | JSON | 形式 |
|---|---|---|
| `info` | ✅ | 单对象 |
| `list` / `ls` / `ps` | ✅ | JSONL |
| `stats` | ✅ | JSONL |
| `images` | ✅ | JSONL |
| `network list` | ✅ | JSONL |
| `volume list` | ✅ | JSONL |
| `inspect` | ✅（默认） | JSON 数组 |
| `system session list` | ❌ | 表格（中文表头） |

---

## 11. 探针复现脚本

```powershell
$env:WSL_UTF8=1
wslc run -d --pull never --name wslc-panel-probe `
     -p 18080:80 -e PROBE=1 docker.1ms.run/library/alpine:latest sleep 300
wslc list -a --format json
wslc stats --format json
wslc inspect -s wslc-panel-probe
wslc stop wslc-panel-probe
wslc list -a --format json            # 采集 exited 状态
wslc remove wslc-panel-probe
```

> 注意：本机直连 Docker Hub 会超时（`registry-1.docker.io` 不可达），
> 必须用**已存在的本地镜像全名** + `--pull never`。
