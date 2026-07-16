# dbtui 架构设计

> 本文是 dbtui 的完整设计与选型文档，涵盖「可直接照着编码」的架构细节（分层、核心抽象、异步事件循环、关键流程、模块职责）与技术选型依据（附录 A）。

---

## 0. 设计目标与约束

### 目标
1. **UI 永不阻塞**：任意耗时的 DB 操作（连接、大查询、schema 自省）都不卡住渲染与键盘响应。
2. **可扩展多数据库**：新增 PG/SQLite 后端只需实现一个 trait，不改动 UI 与事件循环。
3. **可扩展多连接**：同一进程内可同时连多个库（多 Tab），切换无缝。
4. **可测试**：DB 后端、事件处理、组件渲染三者在无终端环境下可独立测试。

### 约束
- 单进程、单主事件循环（Tokio runtime）。
- TUI 占用 stderr，日志只能写文件。
- 用户输入任意 SQL → 结果集结构运行时才知，渲染必须通用化。
- 渲染纪律：`draw` 只读状态，所有变更经事件处理。

---

## 1. 架构分层

### 1.1 四层划分

```
┌─────────────────────────────────────────────────────────────┐
│                     表现层 (Presentation)                    │
│   components/* —— 各面板组件，实现 Component trait           │
│   职责：渲染 + 把终端事件翻译为 Action                       │
├─────────────────────────────────────────────────────────────┤
│                     应用层 (Application)                     │
│   app.rs —— App 状态机，聚合事件、派发 Action、驱动渲染      │
│   event.rs —— Event / Action / DbMessage 定义               │
│   tui.rs —— Terminal 生命周期封装（raw mode / draw）         │
├─────────────────────────────────────────────────────────────┤
│                     数据层 (Data)                            │
│   db.rs —— Database trait（多后端抽象）                      │
│   db/mysql.rs —— MySQL 实现（sqlx MySqlPool）                │
│   职责：连接管理、schema 自省、流式查询                      │
├─────────────────────────────────────────────────────────────┤
│                   基础设施层 (Infrastructure)                │
│   config.rs —— 连接配置（toml + dirs 跨平台路径）            │
│   error.rs —— Error 类型与转换                               │
│   日志 —— tracing + tracing-appender（文件滚动）             │
└─────────────────────────────────────────────────────────────┘
```

### 1.2 依赖方向（严格单向）

```
main.rs
  └─▶ app ──┬─▶ components ─▶ ratatui / crossterm
            ├─▶ db (trait) ─▶ db/mysql ─▶ sqlx
            ├─▶ config ─▶ serde/toml/dirs
            └─▶ error
```

**关键规则**：
- 上层依赖下层 trait，**不**依赖具体实现。`app` 只知 `Database` trait，不知 `MySqlBackend`；具体后端在 `main.rs` 装配。
- `components` **不直接**访问 DB。它产生的 `Action::ExecuteQuery(sql)` 由 `app` 翻译为 DB 任务。
- 跨层通信只走两种通道：**同步**（函数返回值，如 `render`）与**异步消息**（`mpsc` channel）。

---

## 2. 核心抽象

### 2.1 Database trait（数据层抽象）

数据层用 `async_trait` 暴露统一接口（trait 需 `dyn` 多态以支持多后端，原生 `async fn in trait` 当前不利于 `Box<dyn>`）。

> `async-trait` 已纳入附录 A.3 的 `Cargo.toml`（`async-trait = "0.1"`）。其心智简单、自动 boxing future 以满足 `Send`，是 `dyn` 多态场景的标准选择。

```rust
use async_trait::async_trait;
use tokio::sync::mpsc;

#[async_trait]
pub trait Database: Send {
    /// 测试连通性（连接配置校验时调用）
    async fn ping(&self) -> Result<(), DbError>;

    /// 列出所有 schema（MySQL 即 database）
    async fn list_schemas(&self) -> Result<Vec<SchemaInfo>, DbError>;

    /// 列出指定 schema 下的表
    async fn list_tables(&self, schema: &str) -> Result<Vec<TableInfo>, DbError>;

    /// 表结构详情（列名/类型/可空/默认/主键）
    async fn describe_table(&self, schema: &str, table: &str) -> Result<Vec<ColumnInfo>, DbError>;

    /// 执行非查询语句（INSERT/UPDATE/DDL），返回影响行数
    async fn execute(&self, sql: &str) -> Result<ExecResult, DbError>;

    /// 流式查询：结果不直接返回，而是通过 tx 逐页回传 DbMessage::QueryPage，
    /// 最终发一条 DbMessage::QueryComplete。查询可被 cancel。
    async fn query_stream(
        &self,
        sql: &str,
        query_id: QueryId,
        tx: mpsc::Sender<DbMessage>,
    ) -> Result<(), DbError>;

    /// 中止一个进行中的查询（best-effort）
    async fn cancel(&self, query_id: QueryId) -> Result<(), DbError>;
}
```

**设计要点**：
- `query_stream` 把「拉取」与「回传」解耦：后端在 trait 实现内部用 sqlx `.fetch()` 流式拉取，攒满一页（见 §4.4）经 `tx` 发回，避免一次性载入大结果集。
- `query_id` 贯穿全链路，用于过期消息丢弃（§4.5）与取消。
- 后端构造返回 `Arc<dyn Database>`，由 App 持有。

### 2.2 Component trait（表现层抽象）

```rust
/// 只读的应用上下文，避免组件触达整个 App
pub struct AppContext<'a> {
    pub active_connection: Option<&'a ConnectionState>,
    pub focus: Panel,
    pub mode: AppMode,
    pub theme: &'a Theme,
}

pub trait Component {
    /// 渲染：只读 ctx，禁止修改 App 状态
    fn render(&self, frame: &mut Frame, area: Rect, ctx: &AppContext);

    /// 处理终端事件，返回意图（Action）交给 App 决策；不直接执行副作用
    fn handle_event(&mut self, event: &Event, ctx: &AppContext) -> Action;

    /// 是否当前焦点组件（影响边框高亮 / 事件分发）
    fn is_focused(&self) -> bool { false }
}
```

**设计要点**：
- 组件**无副作用**：`handle_event` 只返回 `Action`，真正的 DB 操作/状态切换由 `app` 执行。这让组件可在无 DB、无终端环境下单测（喂 Event、断言 Action）。
- `AppContext` 是只读快照，防止组件越权改全局状态。

### 2.3 事件 / 意图 / 消息三态分离

三类对象职责严格区分，避免「事件里夹带副作用」的混乱：

| 类型 | 产生者 | 消费者 | 语义 |
|---|---|---|---|
| `Event` | 终端 / 定时器 | `app` | **发生了什么**（按键、Resize、Tick） |
| `Action` | 组件 `handle_event` | `app` | **想要做什么**（执行查询、切 Tab、退出） |
| `DbMessage` | DB 后端任务 | `app` | **异步结果到了**（连接成功、一页结果、查询完成） |

```rust
// event.rs —— 完整定义见此，下面是核心骨架

pub enum Event {
    Key(KeyEvent),
    Mouse(MouseEvent),
    Paste(String),
    Resize(u16, u16),
    FocusGained,
    FocusLost,
    Tick,                    // 定时器，驱动状态栏刷新等
}

pub enum Action {
    None,
    Quit,
    RequestRender,                                   // 显式请求重绘
    Focus(Panel),                                    // 焦点切换
    SwitchTab(usize),
    OpenPopup(PopupKind),
    ClosePopup,
    Connect(ConnectionConfig),                       // 新建连接
    Disconnect(ConnectionId),
    ExecuteQuery(String),                            // 执行编辑器当前 SQL
    CancelQuery(QueryId),
    LoadSchema(ConnectionId),                        // 刷新库表树
}

pub enum DbMessage {
    Connected(Result<ConnectionHandle, DbError>),
    SchemaLoaded(ConnectionId, Result<SchemaSnapshot, DbError>),
    QueryStarted(QueryId),
    QueryPage(QueryId, Result<QueryPage, DbError>),  // 一批行
    QueryComplete(QueryId, Result<QueryMeta, DbError>),
    Cancelled(QueryId),
}
```

---

## 3. 数据模型

### 3.1 连接模型

```rust
pub struct ConnectionConfig {          // 配置文件里的一个连接定义
    pub id: ConnectionId,              // 稳定标识（用于多 tab 寻址）
    pub name: String,                  // 显示名
    pub driver: Driver,                // Mysql | Postgres | Sqlite
    pub host: String,
    pub port: u16,
    pub user: String,
    pub password: Option<SecretString>,// 敏感字段，不实现 Display/serialize 明文
    pub database: Option<String>,
    pub tls: TlsMode,                  // Disabled | Preferred | Required
}

pub struct ConnectionHandle {          // 连接成功后 App 持有
    pub id: ConnectionId,
    pub backend: Arc<dyn Database>,
    pub schema_snapshot: Option<SchemaSnapshot>,
}
```

### 3.2 查询结果模型

通用化设计，适配任意 SQL 的结果集：

```rust
pub struct ColumnMeta {
    pub name: String,
    pub type_name: String,             // 原始类型名（如 "VARCHAR(255)"），用于表头 tooltip
    pub kind: CellKind,                // Int | Float | Text | DateTime | Bytes | Null | Unknown
}

pub enum CellValue {                   // 已字符串化的单元格，渲染层直接用
    Null,
    Text(String),
    /// 原始字节无法 utf8 解码时，显示十六进制
    BytesHex(String),
}

pub struct QueryPage {                 // 流式查询的一页
    pub columns: Option<Vec<ColumnMeta>>,  // 仅首页携带，之后为 None
    pub rows: Vec<Vec<CellValue>>,
}

pub struct QueryMeta {                 // 查询完成时的汇总
    pub affected_rows: Option<u64>,
    pub rows_returned: u64,
    pub elapsed: Duration,
    pub truncated: bool,               // 是否因达到上限被截断
}

pub struct ResultSet {                 // App 内累积的完整结果（供 TableState 滚动）
    pub columns: Vec<ColumnMeta>,
    pub rows: Vec<Vec<CellValue>>,
    pub meta: Option<QueryMeta>,
    pub complete: bool,
}
```

**字符串化时机**：在 DB 后端（`db/mysql.rs`）拉取每行时，按 `type_info()` 把 sqlx 值转成 `CellValue`（策略见附录 A.2）。这样 UI 层完全不碰数据库类型，跨后端通用。

### 3.3 应用状态模型

```rust
pub enum AppMode { Normal, Connecting, Popup(PopupKind) }

pub struct App {
    mode: AppMode,
    should_quit: bool,

    // 多连接
    connections: Vec<ConnectionState>, // 每个 tab 一个
    active: usize,                     // 当前 tab 下标

    // 焦点与面板
    focus: Panel,
    components: Components,            // 聚合所有 Component 实例（见 §6）

    // 进行中的查询（query_id -> ResultSet 累积器）
    pending_queries: HashMap<QueryId, ResultSet>,

    // channel
    event_rx: mpsc::Receiver<Event>,
    db_rx: mpsc::Receiver<DbMessage>,
    db_tx: mpsc::Sender<DbMessage>,    // clone 给各 DB 任务

    // 错误/通知（供状态栏 + 弹窗）
    last_error: Option<ErrorDisplay>,
    notice: Option<Notice>,
}
```

---

## 4. 异步运行时与事件循环

### 4.1 任务拓扑

```
                        ┌─────────────────────────────┐
                        │     Tokio Runtime (main)     │
                        └──────────────┬──────────────┘
                                       │
   ┌──────────────────┐   Event    ┌───┴────────────────┐
   │  event_task       │ ────────▶ │  event_tx          │
   │  (Crossterm       │           │  (mpsc<Event>)     │
   │   EventStream +   │           └─────────┬──────────┘
   │   tick interval)  │                     │
   └──────────────────┘                     │
                                            ▼
   ┌──────────────────┐   DbMessage  ┌──────────────────┐
   │  db_task (×N)     │ ───────────▶ │ main select!     │
   │  每个 DB 操作一个 │              │  loop:           │
   │  短生命周期任务   │              │  - recv event    │
   │  持有 backend     │              │  - recv db_msg   │
   │  + db_tx clone    │              │  - tick          │
   └──────────────────┘              │  → update(App)   │
                                     │  → maybe draw    │
                                     └──────────────────┘
```

- **event_task**：长生命周期，单一任务。`tokio::select!` 合并 Crossterm `EventStream` 与 `tokio::time::interval`（Tick），统一发到 `event_tx`。
- **db_task**：短生命周期，每个 DB 操作（connect / query / load_schema）spawn 一个，完成后退出。持有 `Arc<dyn Database>` 与 `db_tx` 的 clone。
- **主循环**：在 main task 上 `select!` 同时等 `event_rx` 与 `db_rx`，是唯一的 `App` 状态修改者（单线程语义，无需对 App 加锁）。

### 4.2 channel 模型

| channel | 方向 | 元素 | 容量 |
|---|---|---|---|
| `event_tx → event_rx` | event_task → main | `Event` | 1024（事件突发） |
| `db_tx → db_rx` | db_task(s) → main | `DbMessage` | 256 |

**容量策略**：bounded channel，满了发-producer 端 `try_send` 失败时丢弃 Tick 等可丢消息（DB 结果消息不可丢，用 `await` 背压）。**绝不**用 unbounded channel——会掩盖背压、放大内存。

### 4.3 主循环伪代码

```rust
async fn run(app: &mut App, terminal: &mut Terminal<CrosstermBackend<Stderr>>) -> Result<()> {
    let mut draw_interval = tokio::time::interval(Duration::from_secs_f64(1.0 / FPS));
    draw_interval.set_missed_tick_behavior(MissedTickBehavior::Skip);

    loop {
        if app.should_quit { break; }

        tokio::select! {
            biased;  // 优先消费事件，避免事件饥饿

            Some(ev) = app.event_rx.recv() => {
                let action = app.dispatch_event(ev);   // 组件 handle_event → Action
                app.apply_action(action).await?;       // 执行副作用（含 spawn db_task）
            }

            Some(msg) = app.db_rx.recv() => {
                app.handle_db_message(msg);            // 更新 App 状态（见 §4.5）
            }

            _ = draw_interval.tick() => {
                terminal.draw(|f| app.render(f))?;     // 只读渲染
            }
        }
    }
    Ok(())
}
```

**`apply_action` 的副作用分类**：
- 纯状态变更（Focus/SwitchTab/OpenPopup）：直接改 App。
- 异步操作（Connect/ExecuteQuery/LoadSchema）：`spawn` 一个 db_task，立即返回；App 进入 `Connecting`/等待状态，由后续 `DbMessage` 驱动完成。

### 4.4 流式查询的分页策略

- DB task 内 `sqlx::query(sql).fetch(&pool)` 得到 `Stream`。
- 首行到达前，先用第一条消息可携带 `columns`；为简化，**第一页消息**的 `QueryPage.columns` 带 `Some(Vec<ColumnMeta>)`，之后页为 `None`。
- 每攒满 `PAGE_SIZE`（默认 100）行发一条 `DbMessage::QueryPage`；流结束发 `QueryComplete(QueryMeta)`。
- 上限保护：累计行数达 `MAX_ROWS`（默认 50000）则停止拉取，`QueryMeta.truncated = true`，状态栏提示「结果已截断」。

### 4.5 过期消息与取消

- 每次 `ExecuteQuery` 生成新 `QueryId`，存入 `pending_queries`。
- 用户切 Tab / 重连 / 发新查询时，旧 `QueryId` 标记为**过期**。后续 `QueryPage`/`QueryComplete` 到达时，若 `query_id` 已不在 `pending_queries` 或属非活动 Tab，直接丢弃——避免「切走又弹回」。
- 用户按取消键 → `Action::CancelQuery(id)` → spawn 一个 db_task 调 `backend.cancel(id)`；后端通过 drop 掉对应的 `Stream` 来中止（sqlx 流 drop 即取消）。

---

## 5. 关键流程时序

### 5.1 启动与连接

```
main()
 ├─ 初始化 tracing（文件 appender）
 ├─ 解析 CLI（clap）：--config <profile-name> 或子命令 connect
 ├─ 读 config.toml（dirs::config_dir + std::fs）
 ├─ enable_raw_mode + enter AlternateScreen
 ├─ spawn event_task
 ├─ 构造 App（空连接列表）
 └─ run(app, terminal)
                 │
                 └─ 用户选连接 → Action::Connect(cfg)
                      └─ apply_action: mode=Connecting, spawn db_task:
                           ├─ MySqlBackend::connect(cfg) → Arc<dyn Database>
                           ├─ tx.send(Connected(Ok(handle)))
                           └─ （同任务内顺带）backend.ping()
```

### 5.2 执行查询（流式）

```
[组件] query_editor.handle_event(Enter) → Action::ExecuteQuery(sql)
   │
[app] apply_action:
   ├─ query_id = QueryId::new()
   ├─ pending_queries.insert(query_id, ResultSet::empty())
   ├─ status: "querying..."
   └─ spawn db_task(backend.clone(), query_id, sql, db_tx.clone()):
         ├─ backend.query_stream(sql, query_id, db_tx).await
         │     └─ 内部：fetch() 流式 → 攒页 → tx.send(QueryPage)
         └─ （流自然结束）tx.send(QueryComplete(meta))

[main loop] 逐条收到 DbMessage::QueryPage → 追加到 ResultSet → 触发重绘
                              DbMessage::QueryComplete → 标记 complete, 状态栏显示 meta
```

### 5.3 错误处理与展示

- 任何 `Result<_, DbError>` 的 `Err` 经 `app.handle_db_message` 转为 `ErrorDisplay`（含用户可读消息 + 技术细节），存入 `app.last_error`。
- 表现层：状态栏显示简短错误（红字）；严重错误弹出 `Popup::Error`，按 `q/Esc` 关闭。
- 错误同时 `tracing::error!` 落文件，便于排查。

### 5.4 关闭与清理

```
Action::Quit / Ctrl+C
 ├─ app.should_quit = true → 主循环退出
 ├─ drain pending db_tasks（等待或取消，设超时）
 ├─ leave AlternateScreen + disable_raw_mode（RAII guard 确保即使 panic 也恢复）
 └─ tracing flush
```

**终端恢复用 RAII**：一个 `TerminalGuard` 在 Drop 时执行恢复，避免 panic 后终端留在 raw mode（这是 TUI 应用的常见坑）。

---

## 6. 模块职责详解

| 文件 | 职责 | 关键类型 |
|---|---|---|
| `main.rs` | 入口：CLI 解析、runtime 启动、依赖装配（构造 MySqlBackend 注入 App）、错误兜底 | `fn main()` |
| `app.rs` | App 状态机、事件派发、Action 执行、render 总入口、DB 任务 spawn | `App`, `AppMode`, `run()` |
| `event.rs` | Event / Action / DbMessage / QueryId / ConnectionId 定义 | 各 enum |
| `tui.rs` | Terminal 封装：raw mode、AlternateScreen、`TerminalGuard`、帧率 | `Tui`, `TerminalGuard` |
| `db.rs` | `Database` trait + 公共数据类型（SchemaInfo/TableInfo/ColumnInfo/CellValue 等） | `Database` |
| `db/mysql.rs` | MySQL 后端实现：连接、自省查询、流式查询、字符串化 | `MySqlBackend` |
| `components.rs` | `Component` trait + `AppContext` + `Panel`/`PopupKind`/`Theme` | `Component` |
| `components/connection_list.rs` | 连接选择面板（启动/多 Tab） | `ConnectionList` |
| `components/schema_tree.rs` | 库/表树导航（ListState） | `SchemaTree` |
| `components/query_editor.rs` | SQL 编辑器（光标/缓冲/历史） | `QueryEditor` |
| `components/result_table.rs` | 结果表（TableState，列宽自适应） | `ResultTable` |
| `components/status_bar.rs` | 底部状态（当前连接/模式/查询耗时/快捷键提示） | `StatusBar` |
| `components/popup.rs` | 通用弹窗（错误/确认/输入/帮助） | `Popup` |
| `config.rs` | 配置读写：`dirs::config_dir()` + toml + 密码不落明文 | `Config`, `ConnectionConfig` |
| `error.rs` | `Error`/`DbError`/`ErrorDisplay` 类型与 `From` 转换 | 各 error |

### 布局组织

- 三栏主布局（`ratatui` 的 `Layout`）：左 = schema_tree，右上 = query_editor / result_table（按模式切换），右下 = status_bar。弹窗覆盖在顶层（`Clear` + 居中 `Block`）。
- 焦点循环：`Tab` 键在 `Panel` 间切换，影响边框高亮与事件路由。

---

## 7. 错误处理策略

### 7.1 错误分层

```
DbError (data layer, thiserror)        ← sqlx::Error + 协议错误
   │ From<sqlx::Error>
Error   (app-wide, thiserror)           ← 聚合 DbError / IoError / ConfigError
   │
ErrorDisplay (presentation)             ← 用户可读消息 + 技术细节，供弹窗/状态栏
```

- **库层用 `thiserror`**：`#[derive(Error)]` 定义强类型错误，`#[from]` 自动转换，避免 `Box<dyn Error>` 丢失类型。
- **应用顶层用 `color-eyre`**：`main` 返回 `color_eyre::Result<()>`，安装 panic hook，崩溃时打印带 backtrace 的诊断报告（写文件，不污染终端）。

### 7.2 错误展示原则
- **可恢复**（连接失败、SQL 语法错）：状态栏提示 + 弹窗，不退出。
- **不可恢复**（配置文件损坏、终端初始化失败）：打印清晰错误后退出。
- **绝不**用 `.unwrap()`/`expect()` 吞错误；DB 层返回 `Result`，UI 层转 `ErrorDisplay`。

---

## 8. 扩展点

### 8.1 新增数据库后端（如 PostgreSQL）

1. `Cargo.toml` 的 sqlx features 加 `"postgres"`。
2. 新建 `db/postgres.rs`，实现 `MySqlBackend` 的 PG 对应物 `PostgresBackend`：连接串、`information_schema`→`pg_catalog` 的元数据查询、流式查询。**这是主要工作量**（方言差异见附录 A.2）。
3. `main.rs` 装配处按 `ConnectionConfig.driver` 分支选择后端构造。
4. UI、事件循环、App **零改动**——这正是 trait 抽象的回报。

### 8.2 新增组件

1. `components/xxx.rs` 实现 `Component`。
2. `Components` 聚合体新增字段；布局 `Layout` 增加区域。
3. `Panel` 枚举新增变体以支持焦点切换。

### 8.3 多连接（多 Tab）

当前 `App.connections: Vec<ConnectionState>` 已是数组结构，多 Tab 是天然支持的：
- `Action::SwitchTab(i)` 切换 `active`。
- 每个连接独立 `Arc<dyn Database>` 与 schema 快照。
- 结果表按 Tab 隔离（切 Tab 时暂存当前 ResultSet，不销毁）。

---

## 9. 非功能性设计

### 9.1 日志
- `tracing` + `tracing-subscriber`（env-filter）+ `tracing-appender`（按日滚动文件）。
- 日志目录：`dirs::config_dir()/dbtui/logs/`。
- TUI 运行期间 stderr 被占用，panic hook 重定向到日志文件 + 备用 tmp 文件。

### 9.2 性能考量
- **大结果集**：流式分页（§4.4）+ 上限保护，内存可控。
- **重绘开销**：Ratatui 双缓冲只发 diff；额外用「脏标记」——无事件/无 DbMessage 时不重绘（`draw_interval` tick 时检查 `app.dirty`）。
- **连接池**：每个 `MySqlBackend` 持有一个 `MySqlPool`（min/max 可配），避免每次查询新建连接。

### 9.3 可测试性
- **DB 后端**：trait + 测试用 mock backend（`MockDatabase` 实现 `Database`），无需真实 MySQL。
- **组件**：`handle_event` 是纯函数（喂 Event/ctx → 断言 Action），单测无需终端。
- **App 状态机**：构造 App + 注入 mock channel，喂 Event/DbMessage，断言状态变迁。
- 集成测试（连真实 MySQL）标记为 `#[ignore]`，CI 可选启用。

---

## 10. 设计决策记录（ADR 摘要）

| # | 决策 | 理由 | 代价 |
|---|---|---|---|
| 1 | DB 后端用 `async_trait` + `Box<dyn Database>` | 多后端 `dyn` 多态，原生 async fn in trait 不利 dyn | 引入 async-trait 依赖、future boxing 极小开销 |
| 2 | 组件返回 Action 而非直接执行副作用 | 可测试、副作用集中到 App、便于单测 | 多一层间接 |
| 3 | 查询用流式分页消息（QueryPage）而非一次性 ResultSet | 大结果集不爆内存、UI 渐进显示 | channel 消息多、需 query_id 过期管理 |
| 4 | bounded channel + 背压 | 防内存失控、暴露真实压力 | 需区分可丢/不可丢消息 |
| 5 | 单元格在 DB 层字符串化为 CellValue | UI 层跨后端通用、不碰数据库类型 | 丢失原始类型（保留 ColumnMeta.kind 补偿） |
| 6 | 终端恢复用 RAII guard | panic 后不留 raw mode | — |

---

## 11. 后续文档

- `docs/roadmap.md`（规划）：MVP 功能范围、里程碑划分。
- `docs/data-flow.md`（可选）：用完整时序图细化单条查询的端到端流转。
- 各模块就绪后补充 `docs/module-*.md` 实现笔记。

---

## 附录 A：技术选型与依赖清单

> 本附录的 feature flag 与默认行为均已对照 docs.rs 对应版本页面核实（Ratatui 0.30.2、Crossterm 0.29、sqlx 0.9.0）。

### A.1 技术栈总览

| 层 | 技术 | 版本 | 用途 |
|---|---|---|---|
| TUI 框架 | [Ratatui](https://ratatui.rs/) | `0.30` | 组件化渲染、双缓冲、状态化组件 |
| 终端后端 | [Crossterm](https://docs.rs/crossterm) | `0.29` | 跨平台原始模式、事件轮询、鼠标/粘贴 |
| 异步运行时 | [Tokio](https://tokio.rs) | `1` | 多线程运行时，承载 DB 查询、事件流、定时器 |
| 数据库驱动 | [sqlx](https://docs.rs/sqlx) | `0.9` | 异步 MySQL 驱动、连接池、流式查询 |
| trait 异步 | [async-trait](https://docs.rs/async-trait) | `0.1` | `Database` trait 的 `dyn` 多态（见 §2.1） |
| 错误处理 | [thiserror](https://docs.rs/thiserror) + [color-eyre](https://docs.rs/color-eyre) | `2` / `0.6` | 库内结构化错误 + 应用侧诊断报告 |
| 序列化/配置 | [serde](https://docs.rs/serde) + [toml](https://docs.rs/toml) + [dirs](https://docs.rs/dirs) | `1` / `0.8` / `6` | 连接配置、跨平台路径解析 |
| 日志 | [tracing](https://docs.rs/tracing) + [tracing-subscriber](https://docs.rs/tracing-subscriber) | `0.1` / `0.3` | 结构化日志、异步友好 |
| CLI 参数 | [clap](https://docs.rs/clap) | `4` | 命令行参数、子命令 |

**MSRV**：取各直接依赖 MSRV 的**上界**，以 `cargo build` 实际通过为准。当前已知约束最强的是 Ratatui 0.30（MSRV `1.88`）；sqlx 0.9 的 MSRV 需在首次构建时验证，若报错则上调 `rust-version`。建议工具链 `>= 1.88`。

### A.2 选型要点

**终端 UI：Ratatui 0.30 + Crossterm 0.29**
- 双缓冲渲染（只发 diff，不闪烁）；`ListState` / `TableState` 状态化组件契合结果表滚动。
- Ratatui 0.30 默认 feature 即引入 `ratatui-crossterm`，`ratatui = "0.30"` 一行即可 `use ratatui::backend::CrosstermBackend`。
- Crossterm 跨平台（Win 7+）、事件完备（键鼠/粘贴/焦点），`event-stream` feature 提供 `EventStream` 接入 Tokio `select!`。

**数据库驱动：sqlx 0.9（+ Tokio）**

针对 TUI 客户端的选型依据：长查询不阻塞 UI（async + channel）、大结果集流式分页（`.fetch()` Stream）、内置连接池、原生动态 SQL、未来低迁移成本（统一 API，仅切 Pool 类型与连接 URL）。

| 维度 | sqlx ✅ | mysql_async | diesel / diesel-async | sea-orm |
|---|---|---|---|---|
| 异步原生 | ✅ Tokio | ✅ Tokio | ⚠️ diesel 同步 | ✅（基于 sqlx） |
| 流式查询 | ✅ `.fetch()` | ❌ 一次性 | ❌ | ✅ |
| 跨数据库 | ✅ MySQL/PG/SQLite | ❌ 仅 MySQL | ✅ | ✅ |
| 动态 SQL | ✅ 原生 | ✅ 原生 | ⚠️ DSL | ⚠️ ORM |
| MySQL backend 维护 | ✅ 活跃 | ✅ 活跃 | ⚠️ **无人维护** | ✅ |
| 二进制大小 | ~2.5 MB | ~2.0 MB | ~3.0 MB | ~3.5 MB |

否决理由：mysql_async（无流式、纯 MySQL 扩展需重写）、diesel（MySQL 后端无人维护）、sea-orm（ORM 过重、仍 RC）、mysql 同步版（阻塞 UI 线程）。

**结果集渲染策略**（通用展示任意 SELECT 结果）
1. **取列定义**：`row.columns()` 给出列名 + `type_info()`，首行到达即可渲染表头。
2. **流式分页**：`StreamExt::take(N)` 按视口批量拉取，配合 `TableState` 滚动，避免大结果集一次性载入。
3. **字符串化**：按 `type_info()` 分支常见类型，其余 fallback 到 lossy-utf8/十六进制；通用显示走 `MySqlValue`→字符串，不强依赖类型匹配。
4. **消息可丢弃**：查询附带 `query_id`，切 Tab/重连后过期消息丢弃。

**多数据库扩展的真正成本 = 元数据查询方言差异**：MySQL 查 `information_schema`、PostgreSQL 查 `pg_catalog`、SQLite 查 `sqlite_master` / `pragma table_info`。连接层 sqlx 已统一。

### A.3 完整 Cargo.toml

```toml
[package]
name = "dbtui"
version = "0.1.0"
edition = "2021"
rust-version = "1.88"            # 取各依赖 MSRV 上界；sqlx 0.9 实际 MSRV 以首次构建为准

[dependencies]
# --- TUI ---
ratatui = "0.30"                 # 默认 feature 已含 crossterm 后端（ratatui-crossterm）+ macros
crossterm = { version = "0.29", features = ["event-stream"] }
futures = "0.3"                  # EventStream / Stream 组合

# --- 异步运行时 ---
tokio = { version = "1", features = ["rt-multi-thread", "macros", "sync", "time", "io-util"] }

# --- 数据库 ---
# sqlx 0.9 把 runtime 与 TLS feature 拆开了，不再有 runtime-tokio-rustls 组合 feature。
# default-features = false 关闭默认的 any/json/macros/migrate，按需补回（不含 migrate）。
sqlx = { version = "0.9", default-features = false, features = [
    "mysql",                     # MySQL 驱动
    "runtime-tokio",             # Tokio 运行时（0.9 起与 TLS 分离）
    "tls-rustls",                # 纯 Rust TLS（webpki 根证书）；自签证书环境改用 tls-rustls-ring-native-roots
    "macros",                    # query! 宏；已内含 offline（编译期免连库）
    "chrono",                    # 时间类型解码
    "uuid",                      # UUID 类型解码
] }
async-trait = "0.1"              # Database trait 的 dyn 多态（见 §2.1）

# --- 错误处理 ---
thiserror = "2"                  # 模块内结构化错误
color-eyre = "0.6"               # 应用顶层诊断报告（带 backtrace）

# --- 配置 / 序列化 ---
serde = { version = "1", features = ["derive"] }
toml = "0.8"                     # 连接配置文件
dirs = "6"                       # 跨平台配置目录（config_dir()）

# --- 日志 ---
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }
tracing-appender = "0.2"         # 滚动日志文件（TUI 占用 stderr，日志写文件）

# --- CLI ---
clap = { version = "4", features = ["derive"] }
```

### A.4 Feature flags 关键说明

- **Ratatui 默认即带后端**：0.30 默认 feature 集含 `crossterm`（引入 `ratatui-crossterm ^0.1.2`），无需把 `ratatui-crossterm` 写进依赖。
- **sqlx feature 拆分**：0.9 不存在 `runtime-tokio-rustls` 组合 feature，须分别启用 `runtime-tokio` + `tls-rustls`。
- **关闭默认 migrate**：`default-features = false` 同时排除默认的 `migrate`——客户端**不应**对用户库执行 migration，这是刻意的。
- **offline 已并入 macros**：无需单独写 `offline`；`macros` 已内含。
- **TLS 选择**：`tls-rustls` = `tls-rustls-ring-webpki`（纯 Rust、编译顺）；遇自签证书切 `tls-rustls-ring-native-roots` 读系统根证书。
- **配置路径**：用 `dirs::config_dir()` 跨平台解析（macOS → `~/Library/Application Support/dbtui/`，Linux → `~/.config/dbtui/`，Windows → `%APPDATA%\dbtui\`），勿硬编码 `~/.config`。

### A.5 参考资料

**Ratatui / Crossterm**
- Ratatui 官网 / API：<https://ratatui.rs/> · <https://docs.rs/ratatui>
- Ratatui 0.30.2 feature flags：<https://docs.rs/crate/ratatui/0.30.2/features>
- 官方模板：<https://github.com/ratatui/templates> · Awesome Ratatui：<https://github.com/ratatui/awesome-ratatui>
- Crossterm 文档：<https://docs.rs/crossterm>

**sqlx / async-trait**
- sqlx 文档：<https://docs.rs/sqlx> · feature flags：<https://docs.rs/crate/sqlx/0.9.0/features> · GitHub：<https://github.com/launchbadge/sqlx>
- async-trait：<https://docs.rs/async-trait>

**架构参考项目**
- [gitui](https://github.com/extrawurst/gitui) — 异步事件循环 + 组件化范本
- [bottom](https://github.com/ClementTsang/bottom) — 定时刷新 + 多面板布局
- [db-client](https://github.com/b4s36t4/db-client) — Ratatui + sqlx 的数据库 TUI，最接近本项目
