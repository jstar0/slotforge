# 预约与候补 — 完整产品设计

目标：本地活动预约服务v1，内部SlotForge + OutboxRelay。Rust/Go核心当前主要是内存引擎与演示，不是已有线上服务。本设计确定真实服务边界。

## 1. 使用场景与范围

校园工作坊/小型活动。参与者找活动→预约或候补→查看我的预约→取消/签到；组织者创建活动、调整容量、查看递补与通知。普通产品默认FIFO；fair仅保留为明确标注的政策实验。
首版无支付、微信、真实邮件、SMS、外部身份提供商，不声称生产认证系统。通知终点是本应用的本地收件箱，预约成功与通知送达分开呈现。
活动时间重叠给用户提示，但不承诺跨活动自动排程。每人最多3条有效预约（含候补），同活动最多1条。

## 2. 先修领域与持久化

- 现有Event仅初始化，没有创建/调容量API。新增booking_close_at（默认starts_at），截止时刻 at>=booking_close_at 拒绝预约，不能继续用ends_at。
- reservation状态保留 confirmed/waitlisted/checked_in/cancelled；活动结束后的“未签到”是派生展示，不自动修改领域历史。
- self取消只允许starts_at前的confirmed/waitlisted；coordinator可在开始前取消指定预约并说明原因；checked_in不能普通取消。签到时间范围starts_at前30分钟至ends_at，服务端决定当前时间。
- 容量不得减到confirmed+checked_in人数以下；增加容量在报名截止前立即逐个递补，保持原候补政策，变更只提交一次。取消释放名额也仅在 at<booking_close_at 时递补；截止后候补不再转确认，界面显示“候补已截止”，保留领域状态和历史。
- EngineSnapshot缺requests、序列和fair grants，不可用作完整恢复。新增版本化PersistedEngineState，覆盖全部行为状态。
- 现有请求fingerprint包含at；服务重试必须复用首次服务端分配时间，不能每次now传回引擎造成同key冲突。
- OutboxRelay旧task.reserved/confirmed模型不是预约通知协议；仅复用投递/去重/恢复思路，不让Go拥有第二份预约状态机。

## 3. 页面和视觉

白色/暖灰 #faf9f6，正文 #25243a，靛蓝 #4945b6。面向参与者用大日期、明确名额和按钮，管理视图才展示表格与投递诊断。参考日历产品的可读节奏，避免交易后台风格。
顶部“活动 / 我的预约 / 通知”；身份区固定标“本地演示身份”。组织者另有“管理”入口。桌面1200居中内容，日期侧栏220、活动列表/详情；390直接日列表卡片，预约操作固定在详情底部。

### 活动 /events
周/日切换、日期与主题筛选；同时提供列表，时间用Asia/Shanghai显示并标时区。活动卡显示开始/结束、位置说明、可预约/已满/已截止、剩余名额。
详情包含说明、容量、截止、候补规则。按钮按真实状态：“预约”“加入候补”“查看我的预约”；满额不伪装失败，不强迫反复点击。
提交期间禁用重复按钮，仍保留同一幂等key供网络重试。成功显示confirmed收据或候补位置；结果未知显示“正在核实预约结果”，通过原key查询，不生成新key重复预约。

### 我的预约 /reservations
当前/历史分组，每条明确活动时间、状态、候补位置和下一步。候补递补后显示已确认；不是只在toast里通知。
取消弹窗说明会释放名额、是否影响其他预约，成功后更新列表。FIFO显示当前队位；fair实验只能显示当前排序、可能变化，不能保证固定位置。
签到入口是活动详情中的按钮/组织者手动签到，首版不做可伪造二维码安全承诺。

### 通知 /notifications
本地站内收件箱，有未读、来源预约、发生时间。预约已确认但通知待重试时，预约卡仍显示成功，并另标“通知同步中”。通知投递失败不回滚预约。
详情可查看预约状态；通知内容按事件生成，不每次打开时根据当前状态改写旧消息。

### 管理 /admin
活动表与创建/编辑抽屉，容量变更、关闭新预约、参与者/候补名单。修改用revision冲突提示，容量减少不会强制退掉别人。
投递页显示待发/处理中/重试/已送达/需处理，尝试次数、下次时间、错误摘要。组织者可手动重试；参与者不可访问。
演示故障开关放在明确的“故障演示”折叠区：投递前失败、收件箱写入后ack前失败。不会伪装真实邮件异常。

## 4. 服务与事务：定稿方案

Rust为唯一预约领域写入者；SQLite WAL文件在runtime，单实例服务加全局命令锁，不宣称水平扩展。
命令步骤：锁内读取已提交state → clone领域状态 → 执行Engine → 开启SQLite事务 → 写新state、外层幂等收据、所有预约/递补变化和outbox → COMMIT → 替换内存。事务失败丢弃clone，内存不得先行成功。
读取与写入保持同一锁策略；重启只读已提交PersistedEngineState。外层commands表唯一(actor_id,request_key)，fingerprint覆盖业务请求，不含每次变化的now；首次时间持久化。引擎request_id使用SHA-256的64字符hex派生key，避免跨actor碰撞/非法字符。
SQLite事务不能跨Rust HTTP与Go HTTP。Go只领取outbox投递，更新投递字段；不会修改预约状态表。outbox租约一次60秒，最多领取20条；租约过期可重新领取。
站内inbox写入与consumer去重(event_id UNIQUE)在同一SQLite事务，之后ack outbox。ack前崩溃会重试，唯一event_id防止重复显示；同id不同payload hash必须拒绝并标需处理。
退避2/4/8/16/32/60秒，默认最多8次，之后dead_letter允许人工重试。实际送达时间单独记录；只承诺至少一次投递+幂等收件箱。

主要表：
- engine_state(id=1,schema_version,revision,payload)；
- commands(actor_id,request_key,fingerprint,assigned_at,response, UNIQUE(actor_id,request_key))；
- outbox(event_id PRIMARY KEY,aggregate_id,aggregate_version,type,payload,payload_sha256,occurred_at,attempts,status,lease_until,next_attempt_at,last_error)；
- inbox(event_id UNIQUE,recipient_id,body,received_at,read_at,payload_sha256)；
- sessions(session_id_hash,actor_id,expires_at)及固定demo actors。
事件：
~~~json
{"schema_version":1,"event_id":"evt_uuid","aggregate_id":"reservation_1","aggregate_version":2,"type":"reservation.promoted","occurred_at":"2026-09-15T01:00:00Z","payload":{"reservation_id":1,"event_id":"workshop_a","attendee_id":"demo_a","status":"confirmed"}}
~~~
事件类型限定 reservation.confirmed/waitlisted/cancelled/promoted/checked_in 与 event.updated。每个业务变更和递补都在原事务产生事件。hash重算实际规范化payload，不只检查hash格式。

## 5. API与身份（拟建）

演示身份由后端固定名册选择，POST /demo/session只接受已配置actor key，颁发HttpOnly SameSite=Strict cookie；后端查角色，不相信前端role/header。只绑定127.0.0.1；不是生产登录。
时间持久化UTC Unix秒，API ISO8601 UTC，UI明确Asia/Shanghai；请求时间由服务器分配。

| API | 行为 |
|---|---|
| GET /events、GET /events/{id} | 公共活动与容量摘要，不暴露他人身份；列表含已占名额和剩余名额 |
| POST /events（coordinator） | title/start/end/booking_close/capacity → revision |
| PATCH /events/{id}（coordinator） | If-Match，容量/active等变更与递补原子提交 |
| POST /reservations | Idempotency-Key + event_id/cohort → confirmed/waitlisted收据 |
| GET /commands/{key} | 本actor请求结果或404，供未知结果恢复 |
| GET /me/reservations | 当前/历史、当前候补位置 |
| POST /reservations/{id}/cancel、check-in | 幂等key；后端所有权/时间/角色校验 |
| GET /me/notifications | 仅本人收件箱；`event_id` 是活动 ID，`delivery_event_id` 是 outbox 消息键；PATCH /notifications/{id}/read |
| GET /admin/events/{id}/roster | 权限过滤后的名单 |
| GET /admin/deliveries、POST /admin/deliveries/{id}/retry | 投递诊断与重试 |
| POST /admin/demo/reset | 明确确认后重置本地合成数据，不能自动重置真实用户导入 |

收据包括reservation_id、event_id、status、waitlist_position（confirmed为null）、replayed、event_revision。缺席/已取消/截止不是网络故障；409幂等冲突解释同key参数不一致。
恢复请求返回原业务结果并标replayed=true，不新增预约/outbox。前端role只决定导航展示，不决定权限。

## 6. 实施顺序

A. 修截止语义、容量调整、可恢复完整state与幂等时间；领域测试先覆盖这些新增行为。
B. Rust SQLite命令事务/恢复 + axum API；真实重启后再次用同key得到同收据。
C. 定义outbox/inbox协议，Go租约/退避/去重，故障注入验证ack前崩溃。
D. 活动/我的预约/通知三个参与者流程，随后管理创建/容量/投递页面。
E. 示例：活动容量2，A/B确认、C候补；A取消→C确认；投递失败→预约保留、重试只出现一条通知。加入两个并发请求抢最后一席。

## 7. 完成要求

容量不超卖、同key不重复、越权无法访问/变更他人预约；候补递补与outbox在同一事务。
数据库故障不提前改变内存；服务重启保留序列、幂等、公平计数，不能从展示快照猜测恢复。
减少容量低于占用量可解释地拒绝；增加容量原子递补。预约在booking_close_at关闭，UI与服务一致。
站内通知重复投递只显示一次，同ID异内容拒绝。清楚区分预约成功、通知待发、站内已送达，不声称邮件送达或exactly-once。
桌面和手机能完成预约/候补/取消；用两种角色实际验证，README从空目录启动两进程后演示成功。
