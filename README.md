# SlotForge
SlotForge 是一个本地活动预约与候补产品：参与者浏览活动、预约或候补、取消和查看站内通知；协调者创建及编辑活动、查看名单和诊断通知投递。Rust 是唯一的预约状态写入者，Go relay 只负责把已经提交的 outbox 事件投递进同一 SQLite 文件的本地 inbox。
