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
