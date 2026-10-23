# PostgreSQL 实测记录（为 v0.14 驱动准备）

> 全部数据来自**真容器**：`docker.1panel.live/library/postgres:16`，
> 在本机用 `wslc run` 起、`wslc exec` 抓。
> 夹具存在 `crates/wslc-core/tests/fixtures/postgres_*.tsv`。
>
> 记这份文档的理由（`AGENTS.md` §5"实测优先于文档"）：
> **`psql` 的输出格式和 `mysql -B` 不一样**，而差异点全是"看文档看不出来、
> 写错了只表现为'结果表里多一行怪东西'"的那类。所以先测，再写驱动。

---

## 一、和 MySQL 的四处**关键差异**

| # | 项目 | `mysql -B` | `psql -A` | 后果 |
|---|---|---|---|---|
| 1 | 默认字段分隔符 | **TAB** | **一个字面 `\|`** | 不显式传 `-F` 就会按 `\|` 切，而数据里常有 `\|` |
| 2 | 表头 | `-B` 自带 | `-A` 带，但**尾部多一行 `(N rows)`** | 不删这行会多出一条假数据行 |
| 3 | DDL 的命令标签 | 空 | **`CREATE TABLE` 这种标签** | 不处理会把 `CREATE TABLE` 当成一行数据 |
| 4 | 错误输出 | `ERROR 1064 (42000): …` | `ERROR: …` + `LINE 1: …` + `^` 指针 | 分类用不了同一套正则 |

**结论：PostgreSQL 驱动必须显式用这套参数：**

```text
psql -X -q -U <user> -A -t -F <TAB> -v ON_ERROR_STOP=1 [-d <db>] -c <sql>
```

`PGPASSWORD` 走环境变量（`wslc exec -e PGPASSWORD=…`，实测可行）。
`-X` 忽略 `~/.psqlrc`（否则用户的 psqlrc 可能改掉输出格式），
`-q` 压掉命令标签，`-A -t -F` 要 TSV，`-v ON_ERROR_STOP=1` 让多语句中途报错就停。

---

## 二、逐条测量

### 1. 客户端版本

```text
$ psql --version
psql (PostgreSQL) 16.15 (Debian 16.15-1.pgdg13+2)
```

### 2. 分隔符默认是 `|` 不是 TAB

不加 `-F` 时：`1|alice||t|{a,b}`。
加 `-F "\t"` 后（**这才是我们要的**）：

```text
1<TAB>alice<TAB><TAB>t<TAB>{a,b}
2<TAB>bob<TAB>小博<TAB>f<TAB>
3<TAB><TAB><TAB>t<TAB>{}
```

### 3. NULL 和空串**都是空**（和 `mysql -B` 一致）

第 1 行 `nick` 是 NULL、第 3 行的 `name` 是 NULL、`nick` 是空串 `''` ——
输出里**完全一样**。

> 这正好符合项目已有的决定：**NULL 与空串不区分**。
> 想区分得传 `-P null='NULL'`，但那样真的 NULL 值会和字符串 `'NULL'` 撞车，
> 得不偿失。

### 4. 带表头时尾部有 `(N rows)`

```text
$ psql -X -q -U postgres -A -F <TAB> -c 'SELECT id, name, nick FROM t_user ORDER BY id;'
id<TAB>name<TAB>nick
1<TAB>alice<TAB>
2<TAB>bob<TAB>小博
3<TAB><TAB><TAB>
(3 rows)          ← 这一行不是数据
```

单复数都出现过：**`(1 row)`** 和 **`(2 rows)`**。所以剥离规则是
`^\(\d+ rows?\)$`。

空结果集时是 `id\n(0 rows)\n`（12 字节）——**表头还在**，说明"零行"和
"没结果集"在带表头模式下可以区分。

### 5. 空结果集（`-A -t`）= **0 字节**

和 MySQL `-B` 完全一样。所以"没有返回行"和"没有结果集"在 `-t` 模式下
仍然分不开 —— UI 那条"执行成功，没有返回行"的文案可以照用。

### 6. 多条语句的输出**直接粘连**

```text
$ psql -X -q -U postgres -A -t -F <TAB> -c 'SELECT 1 AS a; SELECT 2 AS b;'
1
2
```

中间没有任何分隔。**必须客户端切分**（和 MySQL 那边 `sqlstmt` 的理由一模一样）。

### 7. `-q` 压掉 DDL 命令标签（**关键**）

| 语句 | 不带 `-q` | 带 `-q` |
|---|---|---|
| `CREATE TABLE tmp_y (id int)` | `CREATE TABLE` | **0 字节** |
| `DROP TABLE tmp_y` | `DROP TABLE` | **0 字节** |
| `SELECT 7 AS n` | `7` | `7`（不受影响） |

所以 `-q` 是必须的：不压掉的话，"执行一条 CREATE" 会被解析成
"一行一列、值为 `CREATE TABLE` 的结果集"，界面上就会出现一张假表格。

### 8. 错误格式（stderr，退出码 1）

语法错误：

```text
ERROR:  syntax error at or near "SELEC"
LINE 1: SELEC 1;
        ^
```

库不存在：

```text
psql: error: connection to server on socket "/var/run/postgresql/.s.PGSQL.5432"
failed: FATAL:  database "nodb" does not exist
```

> 好消息：`psql` 的报错**比 MySQL 更适合直接给用户看** ——
> 它带 `LINE 1: …` 和插入符，能指到出错位置。MySQL 只有一条消息。

### 9. `wslc exec -e` 能传环境变量

```powershell
wslc exec -e PGPASSWORD=pgpass pgtest psql -U postgres -h 127.0.0.1 -A -t -c 'SELECT 1;'
```

和 MySQL 路径用 `MYSQL_PWD` 是同一个套路。

### 10. ⚠️ 这个容器的 `pg_hba` 是 `trust`

错密码也连上了（`PGPASSWORD=wrong` 依然返回 `1`）。所以**认证失败这条路径
在本机测不出来** —— 写驱动时不能靠"我测过认证失败"来下结论。
（官方镜像的 `pg_hba.conf` 默认策略随 `POSTGRES_HOST_AUTH_METHOD` 变，
本机实测是 trust。）

### 11. 元数据查询（照 MySQL 那套的形状）

库列表：

```sql
SELECT datname FROM pg_database
WHERE datallowconn AND NOT datistemplate
ORDER BY datname;
```

表 / 视图（**注意 PostgreSQL 多一层 schema**）：

```sql
SELECT table_schema, table_name, table_type
FROM information_schema.tables
WHERE table_schema NOT IN ('pg_catalog','information_schema')
ORDER BY table_schema, table_name;
```

实测输出：

```text
public<TAB>t_user<TAB>BASE TABLE
public<TAB>v_user<TAB>VIEW
```

列：

```sql
SELECT table_schema, table_name, column_name
FROM information_schema.columns
WHERE table_schema NOT IN ('pg_catalog','information_schema')
ORDER BY table_schema, table_name, ordinal_position;
```

实测输出：

```text
public<TAB>t_user<TAB>id
public<TAB>t_user<TAB>name
public<TAB>t_user<TAB>nick
public<TAB>t_user<TAB>active
public<TAB>t_user<TAB>tags
public<TAB>t_user<TAB>created
public<TAB>v_user<TAB>id
public<TAB>v_user<TAB>name
```

⚠️ 两点：

1. `table_type` 的值是 `BASE TABLE` / `VIEW`（**带空格**），
   而 MySQL 是 `BASE TABLE` / `VIEW` —— 恰好一致，但**这是我们测出来的巧合**，
   不要写成"标准规定"。
2. 系统 schema 有两个（`pg_catalog` / `information_schema`），
   而 MySQL 是四个系统**库**。排除条件不能照抄。

---

## 三、写驱动时必须守住的三条

1. **显式 `-F "\t"`** —— 默认是 `|`，这条最容易忘，而后果是"所有含 `|` 的值被切错"。
2. **`-q` + `-t`** —— 否则 DDL 会变成一个假结果集。
3. **切分在客户端做** —— 多语句输出粘连，服务端不给分隔。

## 四、还没测的（阶段 2 补）

- 认证失败的真实报错（本机 `trust`，测不出）；要测需要改 `pg_hba.conf` 或
  用 `POSTGRES_HOST_AUTH_METHOD=scram-sha-256` 重起容器；
- 大字段（`text` 超长）、`bytea`（二进制）的输出形态；
- `timestamptz` 的时区呈现（跟容器 `TZ` 有关）；
- 数组 / JSON 类型的输出（本机测到 `{a,b}` 这种 PostgreSQL 数组字面量）。
