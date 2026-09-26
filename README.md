# SlotForge

SlotForge 是一个本地活动预约与候补产品：参与者浏览活动、预约或候补、取消和查看站内通知；协调者创建及编辑活动、查看名单和诊断通知投递。Rust 是唯一的预约状态写入者，Go relay 只负责把已经提交的 outbox 事件投递进同一 SQLite 文件的本地 inbox。

这不是支付、邮件、短信、真实身份或线上多实例服务。演示身份和活动均为合成数据，服务只绑定本机 `127.0.0.1`。

## 启动工作台

需要 Rust 1.80+、Go 1.24+、pnpm。以下命令从一个新的本地 runtime 开始；请保留两个终端同时运行。

终端一：构建工作台并启动 Rust 服务。

```bash
cd slotforge
pnpm --dir web install
pnpm --dir web build
cargo run -- serve --host 127.0.0.1 --port 5204 --runtime-dir runtime/slotforge
```

终端二：启动 relay。

```bash
cd relay
go run ./cmd/slotforge-relay --database ../runtime/slotforge/slotforge.sqlite
```

打开 [http://127.0.0.1:5204/](http://127.0.0.1:5204/)。工作台可在顶部切换四个固定的本地演示身份：参与者 A/B/C 与协调者。Vite 开发时可另开 `pnpm --dir web dev`，它会代理 API 到 5204；验证与演示入口仍是 Rust 的 5204 同源地址。

## 真实流程

1. 以参与者 A/B/C 进入“活动”，预约容量有限的工作坊。前两个获得确认，之后显示候补位置。
2. 从“我的预约”取消一个已确认预约。报名截止前 FIFO 候补会立即递补；截止后保留候补而不伪造确认。
3. 运行 relay 后，进入“通知”查看站内收件箱。预约成功不会等待通知送达。
4. 以协调者进入“管理”：编辑活动、增加容量、查看名单，以及查看 relay 的 pending/retrying/delivered/dead_letter 状态。

活动编辑统一使用 `If-Match` revision；另一个编辑者先保存后，旧草稿得到 409 冲突而不会静默覆盖。容量不能低于已确认或已签到人数；增容发生的递补与活动新 revision 在同一 Rust SQLite 事务中提交。

活动页可按日或按周浏览，日期与主题筛选使用 `Asia/Shanghai`；卡片显示真实已占和剩余名额。若所选活动与本人已确认、候补或签到的活动时间重叠，详情会给出提醒，不会替用户自动排程。我的预约按当前/历史分组；活动结束后保留服务端状态，并以说明展示，不自动改写成缺席。

## 本地故障演示

relay 有两个显式的单次故障参数，用于验证预约与通知投递的边界：

```bash
go run ./cmd/slotforge-relay --database ../runtime/slotforge/slotforge.sqlite --once --fail-before-inbox
go run ./cmd/slotforge-relay --database ../runtime/slotforge/slotforge.sqlite --once --fail-after-inbox-before-ack
```

第二个参数会先提交 inbox，再在 outbox ack 前失败。下一次正常 relay 重试会重新领取 event，但 `inbox.event_id UNIQUE` 与 payload SHA-256 比对保证用户只看见一条通知。失败按 2/4/8/16/32/60 秒退避；第 8 次失败成为 `dead_letter`，协调者可以从管理页重新排队。

## API 摘要

- `POST /api/v1/demo/session`：只接受固定 actor key，发放 `HttpOnly; SameSite=Strict` 本地 cookie。
- `GET/POST /api/v1/events`、`GET/PATCH /api/v1/events/{id}`：公开读取、协调者创建和带 `If-Match` 的编辑。
- `POST /api/v1/reservations`、`GET /api/v1/me/reservations`、取消、签到：所有命令要求 `Idempotency-Key`。
- `GET /api/v1/me/notifications`、`PATCH /api/v1/notifications/{id}/read`：只读取/更新当前 cookie 身份的 inbox。
- `GET /api/v1/admin/events/{id}/roster`、`GET /api/v1/admin/deliveries`、`POST /api/v1/admin/deliveries/{id}/retry`：协调者专属。

## 验证

```bash
cd slotforge
cargo fmt --all -- --check
cargo test --all-targets
cargo clippy --all-targets --all-features -- -D warnings
pnpm --dir web build

cd relay
go test ./...
```

测试覆盖预约关闭、容量与候补、状态恢复、SQLite 命令重开回放、revision 冲突、outbox 与状态同事务提交，以及 relay 在“inbox 已写、ack 前失败”后的唯一通知恢复。自动化结果支持行为验证，不替代浏览器实际流程或外部发布。

### 新 runtime 本地验收

从项目根目录运行：

```bash
./scripts/acceptance.sh
```

脚本会构建工作台、Rust 服务和 Go relay，在新的临时 runtime 启动服务，并实际走过 A/B 确认、C 候补、A 取消后 C 递补、并发争抢最后一个名额、inbox 已写但 ack 前失败后的唯一通知，以及协调者重新投递 dead-letter。验收程序逐项回读 HTTP API 和 SQLite；死信演示只在这个一次性 runtime 中破坏并恢复一条 outbox payload hash，以便让真实 relay 进入 `dead_letter`，不会碰项目已有 runtime。临时数据库留在系统 `TMPDIR` 下便于检查；脚本拒绝复用已有 `slotforge.sqlite`。默认端口 5204 被占用时可设 `SLOTFORGE_ACCEPTANCE_PORT`，不会结束其他进程。

## 设计边界

- SQLite 使用 WAL 与 `busy_timeout=5000`；Rust 进程以一个本地命令锁串行写入，不声称水平扩展或分布式事务。
- Rust 事务无法跨越 Go 的后续投递；保证的是预约状态与 outbox 同时提交、至少一次投递，以及幂等 inbox，不承诺 exactly-once transport。
- 时间在 API/SQLite 中为 UTC Unix 秒，工作台以 `Asia/Shanghai` 显示。
- 不发送邮件、短信或第三方通知，也不含真实用户资料。

## License

MIT. See [LICENSE](LICENSE).
