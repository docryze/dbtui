# dbtui 实施计划 (Roadmap)

> 本文是把 [`architecture.md`](./architecture.md) 落地为可运行代码的分阶段实施计划。架构（分层、trait、数据模型、事件循环）是权威输入，本计划只解决「按什么顺序做、每步做到什么程度算完成」。
>
> 阅读前提：已读过 architecture.md，了解 §1 四层架构、§2 核心 trait、§4 事件循环、§6 模块职责表。

---

## 0. 实施策略

- **自底向上 + 增量可演示**：先工程骨架 → 终端基础 → 数据层 → 连接 → 查询（MVP）→ 浏览 → 打磨。每个里程碑结束有「能在终端看到/做到什么」的可演示产物。
- **测试随开发同步**：trait 与组件天然可单测（architecture §9.3），不攒到最后补测试。
- **依赖驱动排序**：`Database` trait 先于后端实现，后端先于查询流程，事件循环先于所有交互组件。
- **尽早验证未知**：sqlx 0.9 feature / MSRV、async-trait + `dyn` 的 Send 约束、流式查询列元数据时机——这些是 M0/M2 必须先打通的技术验证点。

---

## 1. 里程碑总览

| 里程碑 | 名称 | 核心产物 | 验收（能在终端做到什么） | 预估 |
|---|---|---|---|---|
| **M0** | 工程骨架 | cargo 工程、目录、依赖、TerminalGuard | `cargo run` 进入空白屏，`q` 退出后终端恢复正常 | 0.5 天 |
| **M1** | 终端基础与事件循环 | tui.rs、event.rs、event_task、App 骨架、主循环 | 显示静态三栏布局，键盘/Resize 正确响应 | 1 天 |
| **M2** | 数据层 | error.rs、db.rs(trait+类型)、db/mysql.rs、MockBackend | 单测通过；真实 MySQL `ping`/`list_schemas` 集成测试通过 | 2 天 |
| **M3** | 连接流程 | config.rs、connection_list、Connect 全链路、status_bar | 从配置选连接、建连成功并显示状态 | 1.5 天 |
| **M4** | 查询与结果（MVP） | query_editor、流式 query_stream、ResultTable、ResultSet | 输入 SQL→执行→结果表流式滚动显示 **（MVP 达成）** | 3 天 |
| **M5** | Schema 浏览 | schema_tree、自省接入、表结构查看 | 浏览库/表树，查看任意表结构 | 1.5 天 |
| **M6** | 打磨与生产化 | 错误弹窗、日志、多 Tab、取消查询、性能优化、集成测试 | 错误优雅展示、日志落盘、大结果集流畅、`Ctrl+C` 中止 | 2.5 天 |

**MVP 定义**：**M0–M4 完成**即最小可用产品——能连 MySQL、执行任意 SQL、流式查看结果。M5/M6 是体验完善。

**总预估**：约 12 人天（单人，含调试与测试）。

---

## 2. 依赖关系与并行机会

```
M0 ──▶ M1 ──┬──▶ M2 ──┬──▶ M3 ──▶ M4 (MVP) ──▶ M6
            │          │
            │          └──▶ (M3 的 config.rs 可与 M2 并行)
            │
            └──▶ (组件 trait §2.2 在 M1 定，组件实现按里程碑渐进)
```

**关键依赖**：
- M1（事件循环）是所有交互的前置。
- M2 的 `Database` trait 是 M3/M4/M5 的共同前置。
- M3 的 `config.rs` 与 M2 的数据层**无耦合，可并行**。

**可并行项**：
- M2 期间，另一人可先做 `config.rs` + `connection_list` 组件骨架。
- M4 的 `result_table` 与 M5 的 `schema_tree` 都只依赖组件 trait（M1 已定）+ 各自数据，可并行实现。

---

## 3. 各里程碑详解

### M0 · 工程骨架

**目标**：建立可编译、可运行的 cargo 工程，跑通最小 TUI 生命周期（进 raw mode → 空白屏 → 退出恢复），把所有技术验证风险在此清零。

**任务**：
- [ ] `cargo init`，按 architecture §6 建目录骨架（`src/{app,event,tui,db,components,config,error}.rs` + `db/` + `components/`，先建空文件/占位模块）。
- [ ] 填入附录 A.3 的完整 `Cargo.toml`（含 async-trait）。
- [ ] **技术验证①**：`cargo build` 通过——确认 sqlx 0.9 feature 拼写、MSRV 满足、无版本冲突。若 sqlx 报 MSRV，上调 `rust-version` 并记录。
- [ ] 实现 `tui.rs` 的 `TerminalGuard`（RAII：new 时 `enable_raw_mode` + `EnterAlternateScreen`，Drop 时反向恢复）。
- [ ] `main.rs`：初始化 `color_eyre`、构造 `Terminal`、用 `TerminalGuard` 包裹、空循环等待 `q`。
- [ ] 加 `.gitignore`（`/target`、`Cargo.lock` 保留）、`rust-toolchain.toml`（pin `>= 1.88`）。
- [ ] CI：GitHub Actions（或本地 just/cargo-make）跑 `cargo fmt --check && cargo clippy -- -D warnings && cargo test`。

**验收**：
- `cargo run` 进入全屏空白（AlternateScreen），按 `q` 干净退出，终端恢复正常。
- 故意 panic（临时加一行）后，终端**仍恢复正常**（验证 TerminalGuard 的 Drop）。
- `cargo clippy` 零警告。

**风险验证（本里程碑必须消除）**：sqlx 0.9 编译、MSRV、依赖版本冲突。

---

### M1 · 终端基础与事件循环

**目标**：搭好异步事件循环骨架（architecture §4），能渲染静态三栏布局并正确响应键盘/Resize，为后续所有交互组件铺路。

**任务**：
- [ ] `event.rs`：定义 `Event`（Key/Mouse/Paste/Resize/FocusGained/FocusLost/Tick）、`Action`（先定义 §2.3 骨架：None/Quit/RequestRender/Focus/SwitchTab/OpenPopup/ClosePopup/Connect/Disconnect/ExecuteQuery/CancelQuery/LoadSchema）、`DbMessage`、`QueryId`/`ConnectionId` 类型。
- [ ] `event_task`（architecture §4.1）：`tokio::spawn` 一个任务，`select!` 合并 Crossterm `EventStream` + `tokio::time::interval`(Tick)，发到 `event_tx`（bounded 1024）。
- [ ] `app.rs`：`App` 结构骨架（mode/focus/should_quit + 两个 channel 末端）、`run()` 主循环（§4.3 伪代码）、`dispatch_event`/`apply_action` 框架。
- [ ] `components.rs`：定义 `Component` trait、`AppContext`、`Panel`、`PopupKind`、`Theme`。
- [ ] 实现 `status_bar` 组件（静态显示 "dbtui · ready · q:quit"），验证 render 闭包只读纪律。
- [ ] `tui.rs`：主循环里的 `draw_interval`（FPS 限制 + 脏标记 `app.dirty`）。
- [ ] 处理 `Event::Resize` → 通知 terminal；`Action::Quit` → `should_quit`。

**验收**：
- 显示三栏静态布局（左/右上/右下，边框可见），状态栏在右下显示 "ready"。
- 按 `q` 或 `Ctrl+C` 退出；调整终端窗口大小，布局正确重绘不错乱。
- Tab 键焦点切换（即使内容为空，边框高亮变化可见）。

---

### M2 · 数据层（Database trait + MySQL 后端）

**目标**：实现数据层抽象与 MySQL 后端，打通「连接 + 自省」，全部可单测。本里程碑是技术含量最高的，async-trait + `dyn` + sqlx 流式都在此验证。

**任务**：
- [ ] `error.rs`：`DbError`（`#[derive(thiserror::Error)]`，`#[from] sqlx::Error`）、`Error`（聚合 DbError/Io/Config）、`ErrorDisplay`。
- [ ] `db.rs`：
  - [ ] 公共类型：`SchemaInfo`/`TableInfo`/`ColumnInfo`/`CellKind`/`CellValue`/`QueryPage`/`QueryMeta`/`ResultSet`/`ExecResult`（architecture §3.2）。
  - [ ] `Database` async trait（§2.1，`#[async_trait]`）：`ping`/`list_schemas`/`list_tables`/`describe_table`/`execute`/`query_stream`/`cancel`。
  - [ ] **技术验证②**：`async-trait` + `Box<dyn Database>` 在 `tokio::spawn` 里 Send 通过。
- [ ] `db/mysql.rs`：`MySqlBackend` 持有 `MySqlPool`：
  - [ ] `connect(cfg) -> Result<Arc<dyn Database>>`（`MySqlPoolOptions`）。
  - [ ] `ping` / `list_schemas` / `list_tables` / `describe_table`（写 `information_schema` 查询，用 `sqlx::query_as` + 手动 row 映射，**不用** `query!` 宏以避开编译期连库）。
  - [ ] `query_stream`：内部 `sqlx::query(sql).fetch(&pool)`，攒满 `PAGE_SIZE`(100) 行发 `DbMessage::QueryPage`，流结束发 `QueryComplete`；累计达 `MAX_ROWS`(50000) 截断。
  - [ ] 单元格字符串化：按 `type_info()` 分支 → `CellValue`（§3.2 + 附录 A.2 策略）。
  - [ ] **技术验证③**：首行 `row.columns()` 能否在流首条消息拿到列定义（决定表头渲染时机）。
- [ ] 测试：`MockBackend`（实现 `Database`，返回预设数据）单测；真实 MySQL 的 `ping`/`list_schemas` 标 `#[ignore]` 集成测试，本地 `cargo test -- --ignored` 跑。

**验收**：
- `cargo test`（含 mock）全绿。
- `cargo test -- --ignored` 连本地 MySQL，`ping` 成功、`list_schemas` 返回预期库名。
- 流式 demo（临时 main 或 example）：对一张表 `SELECT *`，逐页打印行数，证明流式 + 列定义可用。

**风险验证（本里程碑必须消除）**：async-trait dyn Send、流式列元数据时机、sqlx 0.9 API 细节。

---

### M3 · 连接流程

**目标**：打通「读配置 → 选连接 → 异步建连 → 状态反馈」全链路，用户能成功连上一个 MySQL。

**任务**：
- [ ] `config.rs`：`ConnectionConfig`/`Config`/`Driver`/`TlsMode`/`SecretString`；用 `dirs::config_dir()` 定位、`toml` 读写 `connections.toml`；密码字段不实现明文 Display。
- [ ] `connection_list` 组件：渲染连接列表（`ListState`），选中后回 `Action::Connect(cfg)`。
- [ ] `app.rs` 的 `apply_action(Connect)`：mode→Connecting，spawn db_task 调 `MySqlBackend::connect` + `ping`，结果经 `DbMessage::Connected` 回传。
- [ ] `handle_db_message(Connected)`：成功则存 `ConnectionHandle` 进 `connections`、切到 Normal、刷新 schema；失败则 `last_error` + 状态栏红字。
- [ ] `status_bar` 接入：显示当前连接名 / "connecting..." / 错误摘要。
- [ ] 装配：`main.rs` 按 `ConnectionConfig.driver` 选后端（当前仅 MySQL 分支）。

**验收**：
- 启动时若无配置文件，引导用户创建（或在 `connection_list` 提示路径）。
- 配置好一个 MySQL 连接后，选中 → 状态栏显示 "connecting..." → 成功后显示连接名 + "connected"，失败显示错误且不崩溃。
- 连真实 MySQL 成功。

**依赖**：M2（backend）、M1（事件循环 + status_bar）。`config.rs` 可在 M2 期间并行先做。

---

### M4 · 查询执行与结果展示（MVP 核心）

**目标**：实现查询编辑器 + 流式结果表，完成「输入 SQL → 看结果」核心闭环。**本里程碑结束即 MVP。**

**任务**：
- [ ] `query_editor` 组件：多行文本缓冲、光标移动、`Enter` 提交（或 `Ctrl+Enter`/`F5` 执行，避免与换行冲突）、SQL 历史（↑↓ 翻历史）。
- [ ] `app.rs` 的 `apply_action(ExecuteQuery(sql))`：生成 `QueryId`、`pending_queries.insert`、spawn db_task 调 `backend.query_stream`。
- [ ] `handle_db_message(QueryPage)`：首页带 `columns` → 初始化 `ResultSet.columns`；后续页追加 rows；置 `dirty`。
- [ ] `handle_db_message(QueryComplete)`：写 `ResultSet.meta`、`complete=true`、状态栏显示耗时/行数/是否截断。
- [ ] `result_table` 组件：`TableState` 滚动、列宽自适应（按列名 + 采样行宽取 max，有上限）、表头固定、方向键/j-k 滚动。
- [ ] 布局切换：Normal 模式右上区显示 `result_table`；查询中显示 spinner/"querying..."。
- [ ] 过期消息丢弃（architecture §4.5）：切连接/重查后旧 `query_id` 的消息忽略。
- [ ] 非查询语句（INSERT/UPDATE/DDL）：走 `execute`，状态栏显示影响行数。

**验收**（= MVP 验收）：
- 连接后输入 `SELECT * FROM <某表>`，回车执行，结果**流式逐页**填入表格，可上下滚动。
- 大表（如 5 万行）查询不卡 UI、内存可控、达到上限显示「已截断」。
- `INSERT/UPDATE` 显示影响行数；SQL 语法错误在状态栏/弹窗提示且不崩溃。

**依赖**：M3（已连接）、M2（query_stream + 字符串化）。

---

### M5 · Schema 浏览

**目标**：左侧库/表树可导航、可查看任意表结构，让客户端「可浏览」而非「只能盲查」。

**任务**：
- [ ] `schema_tree` 组件：两级树（schema → tables），`ListState` 导航，展开/折叠。
- [ ] 连接成功后自动 `Action::LoadSchema` → spawn db_task 调 `list_schemas` + 各 schema 的 `list_tables`，回 `DbMessage::SchemaLoaded`。
- [ ] `handle_db_message(SchemaLoaded)`：填 `ConnectionHandle.schema_snapshot`、刷新树。
- [ ] 选中表按回车 → `describe_table` → 用 `popup` 展示列结构（列名/类型/可空/键/默认）。
- [ ] 从树中选表 → 可快捷填充到 `query_editor`（如生成 `SELECT * FROM tbl` 骨架）。

**验收**：
- 连接后左侧自动列出所有库，展开某库列出其表。
- 选中表回车弹出表结构详情；关闭弹窗回到树。
- 选表可一键生成查询骨架到编辑器。

**依赖**：M3（连接）、M2（list_schemas/list_tables/describe_table）。

---

### M6 · 打磨与生产化

**目标**：把 MVP 从「能用」提升到「好用」——错误体验、可观测性、健壮性、性能。

**任务**：
- [ ] **错误体验**：`popup` 实现 Error/Confirm/Input/Help 四种；严重错误弹窗带技术细节（可展开）；可恢复错误状态栏红字 + 自动淡出。
- [ ] **日志**：`tracing` + `tracing-appender` 按日滚动文件到 `config_dir()/dbtui/logs/`；`color-eyre` panic hook 重定向到日志文件。
- [ ] **多 Tab**：`SwitchTab` 完整实现，每连接独立 ResultSet/schema，切 Tab 暂存不销毁。
- [ ] **查询取消**：`Action::CancelQuery` → spawn db_task 调 `backend.cancel`（drop sqlx Stream）；状态栏显示取消结果。
- [ ] **性能**：脏标记重绘（无事件不重绘）；大结果集虚拟化（只渲染可视区行，非全量）。
- [ ] **快捷键**：`?` 帮助弹窗、`Ctrl+C` 中止当前查询、`Tab/Shift+Tab` 焦点循环、`1-9` 切 Tab。
- [ ] **集成测试**：真实 MySQL 的查询/自省端到端测试（`#[ignore]`，CI 可选）；App 状态机单测（喂 Event/DbMessage 断言状态）。
- [ ] **README**：安装、配置示例、快捷键表、已知限制。

**验收**：
- 触发各类错误（断网、坏 SQL、权限不足）均有清晰提示，终端不残留 raw mode。
- `logs/` 下有按日日志文件，记录查询与错误。
- 同时连 2 个库，Tab 切换流畅、结果隔离。
- 10 万行查询中途 `Ctrl+C` 能中止，UI 不卡死。

**依赖**：M4（在 MVP 之上打磨）。

---

## 4. 测试策略（呼应 architecture §9.3）

| 层 | 方式 | 时机 |
|---|---|---|
| 数据层 | `MockBackend` 实现 `Database` 返回预设数据；真实 MySQL 用 `#[ignore]` 集成测试 | M2 起，每个后端方法同步写 |
| 组件 | 纯函数单测：构造组件 + `AppContext` mock → 喂 `Event` → 断言返回的 `Action` | 实现组件时同步 |
| App 状态机 | 构造 `App` + 注入 mock channel → 喂 `Event`/`DbMessage` → 断言状态变迁 | M3 起 |
| 端到端 | 真实 MySQL 连接 + 查询全流程，`#[ignore]`，本地/CI 可选 | M4/M6 |

**原则**：DB 层和组件层**必须**可无终端、无真实 DB 单测——这是 trait 抽象的直接回报。

---

## 5. 关键风险与缓解

| 风险 | 影响 | 缓解 | 验证时机 |
|---|---|---|---|
| sqlx 0.9 feature/MSRV 与预期不符 | 编译失败 | M0 立即 `cargo build`，记录真实 MSRV | M0 |
| `async-trait` + `Box<dyn Database>` 不 Send | spawn 失败 | M2 写最小 spawn 验证用例 | M2 |
| 流式 `row.columns()` 时机不确定 | 表头渲染延迟/缺失 | M2 demo 验证首行列定义可用性；不行则 fallback 先 `describe` | M2 |
| 终端 panic 残留 raw mode | 终端变乱码 | `TerminalGuard` RAII + panic hook 测试 | M0 |
| 大结果集内存/卡顿 | UI 不响应 | 流式分页 + 上限 + 虚拟化渲染 | M4/M6 |
| sqlx 连真实自签证书 MySQL | 连接失败 | 文档备选 `tls-rustls-ring-native-roots` | M3 |

---

## 6. 后续（MVP 之后）

- 多数据库：按 architecture §8.1 加 `db/postgres.rs`（UI 零改动）。
- SQL 高亮：编辑器接入 `syntect`。
- 结果导出：CSV/JSON 导出当前 ResultSet。
- 查询历史持久化：存配置目录，跨会话可用。
- 主题：`Theme` 已抽象，补 toml 主题文件加载。

---

## 7. 文档维护

- 每个里程碑结束，回填 architecture.md §6 模块表里该模块的「实现笔记」链接（若偏离设计）。
- 重大设计变更走 architecture.md §10 的 ADR 记录，保持文档与代码一致。
