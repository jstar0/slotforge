import { FormEvent, useEffect, useMemo, useState } from "react";
import { createRoot } from "react-dom/client";
import "./style.css";

type Role = "attendee" | "coordinator" | "observer";
type Identity = "demo-a" | "demo-b" | "demo-c" | "demo-coordinator";
type Event = { event_id: string; title: string; capacity: number; starts_at: number; ends_at: number; booking_close_at: number; active: boolean; revision: number; confirmed_count?: number; remaining_capacity?: number };
type ReservationStatus = "confirmed" | "waitlisted" | "checked_in" | "cancelled";
type Reservation = { reservation_id: number; event_id: string; attendee_id: string; cohort: string; requested_at: number; confirmed_at: number | null; checked_in_at: number | null; status: ReservationStatus };
type Receipt = { reservation_id: number; event_id: string; status: ReservationStatus; waitlist_position: number | null; replayed: boolean; event_revision: number };
type Notification = { notification_id: number; event_id: string; delivery_event_id: string; body: string; received_at: number; read_at: number | null };
type Delivery = { event_id: string; aggregate_id: string; event_type: string; recipient_id: string; attempts: number; status: string; next_attempt_at: number; last_error: string | null; delivered_at: number | null };
type UpdateReceipt = { event: Event; promoted: number[]; replayed: boolean };
type View = "events" | "reservations" | "notifications" | "admin";

class ApiError extends Error { constructor(readonly status: number, message: string) { super(message); } }
const api = async <T,>(path: string, init?: RequestInit): Promise<T> => {
  const response = await fetch(`/api/v1${path}`, { credentials: "same-origin", ...init });
  if (response.status === 204) return undefined as T;
  const body = await response.json().catch(() => ({}));
  if (!response.ok) throw new ApiError(response.status, body.error?.message ?? "请求未完成");
  return body as T;
};
const key = (prefix: string) => `${prefix}-${crypto.randomUUID()}`;
const date = (seconds: number) => new Intl.DateTimeFormat("zh-CN", { dateStyle: "medium", timeStyle: "short", timeZone: "Asia/Shanghai" }).format(new Date(seconds * 1000));
const localInput = (seconds: number) => { const d = new Date(seconds * 1000); const p = new Intl.DateTimeFormat("sv-SE", { timeZone: "Asia/Shanghai", year: "numeric", month: "2-digit", day: "2-digit", hour: "2-digit", minute: "2-digit", hour12: false }).formatToParts(d); const get = (name: string) => p.find((part) => part.type === name)?.value ?? "00"; return `${get("year")}-${get("month")}-${get("day")}T${get("hour")}:${get("minute")}`; };
const epoch = (value: string) => Math.floor(new Date(`${value}:00+08:00`).getTime() / 1000);
const localDay = (seconds: number) => localInput(seconds).slice(0, 10);
const todayInZone = () => localDay(Math.floor(Date.now() / 1000));
const utcDate = (day: string) => { const [year, month, date] = day.split("-").map(Number); return new Date(Date.UTC(year, month - 1, date)); };
const dayKey = (value: Date) => value.toISOString().slice(0, 10);
const shiftDay = (day: string, amount: number) => { const value = utcDate(day); value.setUTCDate(value.getUTCDate() + amount); return dayKey(value); };
const weekStart = (day: string) => { const value = utcDate(day); value.setUTCDate(value.getUTCDate() - (value.getUTCDay() + 6) % 7); return dayKey(value); };
const dateHeading = (day: string) => new Intl.DateTimeFormat("zh-CN", { timeZone: "UTC", year: "numeric", month: "long", day: "numeric" }).format(utcDate(day));
const label: Record<ReservationStatus, string> = { confirmed: "已确认", waitlisted: "候补中", checked_in: "已签到", cancelled: "已取消" };

function App() {
  const [identity, setIdentity] = useState<Identity>("demo-a");
  const [role, setRole] = useState<Role | null>(null);
  const [view, setView] = useState<View>("events");
  const [events, setEvents] = useState<Event[]>([]);
  const [selected, setSelected] = useState<string | null>(null);
  const [reservations, setReservations] = useState<Reservation[]>([]);
  const [notifications, setNotifications] = useState<Notification[]>([]);
  const [deliveries, setDeliveries] = useState<Delivery[]>([]);
  const [retryingDelivery, setRetryingDelivery] = useState<string | null>(null);
  const [notice, setNotice] = useState("选择一个本地演示身份后开始。");
  const [busy, setBusy] = useState(false);
  const [filter, setFilter] = useState("");
  const [calendarMode, setCalendarMode] = useState<"day" | "week">("week");
  const [calendarDay, setCalendarDay] = useState(todayInZone);
  const [nowSeconds, setNowSeconds] = useState(() => Math.floor(Date.now() / 1000));
  const [eventDraft, setEventDraft] = useState<Event | null>(null);
  const [newEvent, setNewEvent] = useState(() => newDraft());

  const rangeStart = calendarMode === "day" ? calendarDay : weekStart(calendarDay);
  const rangeEnd = calendarMode === "day" ? calendarDay : shiftDay(rangeStart, 6);
  const visibleEvents = useMemo(() => events
    .filter((event) => localDay(event.starts_at) <= rangeEnd && localDay(event.ends_at) >= rangeStart)
    .filter((event) => `${event.title} ${event.event_id}`.toLowerCase().includes(filter.trim().toLowerCase()))
    .sort((a, b) => a.starts_at - b.starts_at), [events, filter, rangeEnd, rangeStart]);
  const chosen = visibleEvents.find((event) => event.event_id === selected) ?? visibleEvents[0] ?? null;
  const reservationEvent = (reservation: Reservation) => events.find((event) => event.event_id === reservation.event_id);
  const currentReservations = useMemo(() => reservations
    .filter((item) => item.status !== "cancelled" && (reservationEvent(item)?.ends_at ?? Number.MAX_SAFE_INTEGER) >= nowSeconds)
    .sort((a, b) => (reservationEvent(a)?.starts_at ?? 0) - (reservationEvent(b)?.starts_at ?? 0)), [reservations, events, nowSeconds]);
  const historicalReservations = useMemo(() => reservations
    .filter((item) => !currentReservations.some((current) => current.reservation_id === item.reservation_id))
    .sort((a, b) => (reservationEvent(b)?.starts_at ?? 0) - (reservationEvent(a)?.starts_at ?? 0)), [reservations, events, currentReservations]);
  const bookedEventIds = new Set(reservations
    .filter((item) => ["confirmed", "waitlisted", "checked_in"].includes(item.status))
    .map((item) => item.event_id));
  const overlappingEvents = chosen ? events.filter((event) => event.event_id !== chosen.event_id
    && bookedEventIds.has(event.event_id)
    && event.starts_at < chosen.ends_at
    && chosen.starts_at < event.ends_at) : [];
  const refresh = async (nextRole = role) => {
    const eventList = await api<Event[]>("/events"); setEvents(eventList); setSelected((current) => current && eventList.some((event) => event.event_id === current) ? current : eventList[0]?.event_id ?? null);
    if (nextRole) { setReservations(await api<Reservation[]>("/me/reservations")); setNotifications(await api<Notification[]>("/me/notifications")); }
    if (nextRole === "coordinator") setDeliveries(await api<Delivery[]>("/admin/deliveries"));
  };
  const chooseIdentity = async (next: Identity) => {
    setBusy(true); setIdentity(next);
    try { const session = await api<{ actor_id: string; role: Role }>("/demo/session", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ actor_key: next }) }); setRole(session.role); await refresh(session.role); setView("events"); setNotice(`正在以 ${session.actor_id}（${session.role === "coordinator" ? "协调者" : "参与者"}）查看本地演示数据。`); } catch (error) { setNotice(error instanceof Error ? error.message : "无法建立本地演示会话"); } finally { setBusy(false); }
  };
  useEffect(() => { void chooseIdentity("demo-a"); }, []);
  useEffect(() => { if (chosen) setEventDraft({ ...chosen }); }, [chosen?.event_id, chosen?.revision]);
  useEffect(() => {
    const timer = window.setInterval(() => setNowSeconds(Math.floor(Date.now() / 1000)), 30_000);
    return () => window.clearInterval(timer);
  }, []);
  useEffect(() => {
    if (visibleEvents.length === 0) { if (selected !== null) setSelected(null); return; }
    if (!visibleEvents.some((event) => event.event_id === selected)) setSelected(visibleEvents[0].event_id);
  }, [selected, visibleEvents]);

  const reserve = async () => {
    if (!chosen) return; setBusy(true);
    try { const receipt = await api<Receipt>("/reservations", { method: "POST", headers: { "content-type": "application/json", "idempotency-key": key("reserve") }, body: JSON.stringify({ event_id: chosen.event_id, cohort: "student" }) }); await refresh(); setNotice(receipt.status === "confirmed" ? `预约成功，席位 #${receipt.reservation_id} 已确认。` : `活动已满，已进入候补第 ${receipt.waitlist_position} 位。`); setView("reservations"); } catch (error) { setNotice(error instanceof Error ? error.message : "预约未完成"); } finally { setBusy(false); }
  };
  const cancel = async (reservation: Reservation) => {
    setBusy(true); try { const receipt = await api<{ promoted: number[] }>(`/reservations/${reservation.reservation_id}/cancel`, { method: "POST", headers: { "idempotency-key": key("cancel") } }); await refresh(); setNotice(receipt.promoted.length ? `已取消；候补 #${receipt.promoted.join("、#")} 已递补。` : "已取消预约。"); } catch (error) { setNotice(error instanceof Error ? error.message : "取消未完成"); } finally { setBusy(false); }
  };
  const saveEvent = async (event: Event) => {
    setBusy(true); try { const result = await api<UpdateReceipt>(`/events/${event.event_id}`, { method: "PATCH", headers: { "content-type": "application/json", "idempotency-key": key("event-edit"), "if-match": String(event.revision) }, body: JSON.stringify({ title: event.title, capacity: event.capacity, starts_at: event.starts_at, ends_at: event.ends_at, booking_close_at: event.booking_close_at, active: event.active }) }); await refresh(); setEventDraft(result.event); setNotice(result.promoted.length ? `已保存 revision ${result.event.revision}；候补 #${result.promoted.join("、#")} 已确认。` : `已保存 revision ${result.event.revision}。`); } catch (error) { if (error instanceof ApiError && error.status === 409) { setNotice("活动已被另一位编辑者更新。草稿未覆盖新版本，请重新打开后合并修改。"); } else setNotice(error instanceof Error ? error.message : "保存未完成"); } finally { setBusy(false); }
  };
  const createEvent = async (event: typeof newEvent) => {
    setBusy(true); try { await api<Event>("/events", { method: "POST", headers: { "content-type": "application/json", "idempotency-key": key("event-create") }, body: JSON.stringify({ event_id: event.event_id, title: event.title, capacity: event.capacity, starts_at: epoch(event.starts_at), ends_at: epoch(event.ends_at), booking_close_at: epoch(event.booking_close_at) }) }); await refresh(); setNewEvent(newDraft()); setNotice("活动已创建，可在列表中继续编辑。 "); } catch (error) { setNotice(error instanceof Error ? error.message : "创建未完成"); } finally { setBusy(false); }
  };
  const markRead = async (item: Notification) => { try { await api<void>(`/notifications/${item.notification_id}/read`, { method: "PATCH" }); await refresh(); } catch (error) { setNotice(error instanceof Error ? error.message : "通知状态未更新"); } };
  const retryDelivery = async (item: Delivery) => { if (retryingDelivery) return; setRetryingDelivery(item.event_id); try { await api<void>(`/admin/deliveries/${item.event_id}/retry`, { method: "POST" }); await refresh("coordinator"); setNotice("已重新排队该死信；relay 下次轮询会重新投递。 "); } catch (error) { setNotice(error instanceof Error ? error.message : "无法重新投递"); } finally { setRetryingDelivery(null); } };
  const configureFault = async (mode: "before_inbox" | "after_inbox_before_ack") => { try { await api<void>("/admin/demo/relay-fault", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ mode }) }); setNotice(mode === "before_inbox" ? "已设置下一次 relay 在 inbox 写入前失败。" : "已设置下一次 relay 在 inbox 写入后、ack 前失败。"); } catch (error) { setNotice(error instanceof Error ? error.message : "无法设置本地故障演示"); } };
  const changeDraft = <K extends keyof Event>(field: K, value: Event[K]) => setEventDraft((current) => current ? { ...current, [field]: value } : current);
  const availabilityFor = (event: Event) => {
    if (event.ends_at <= nowSeconds) return { kind: "ended", text: "活动已结束" };
    if (!event.active) return { kind: "closed", text: "活动已关闭" };
    if (nowSeconds >= event.booking_close_at) return { kind: "closed", text: "报名已截止" };
    if (event.remaining_capacity === 0) return { kind: "full", text: "名额已满 · 可加入候补" };
    return { kind: "open", text: `可预约 · 剩余 ${event.remaining_capacity ?? event.capacity} 席` };
  };
  const renderReservation = (reservation: Reservation, historical: boolean) => {
    const event = reservationEvent(reservation);
    const canCancel = !historical && event && event.starts_at > nowSeconds && ["confirmed", "waitlisted"].includes(reservation.status);
    return <article key={reservation.reservation_id} className="reservation">
      <span className={`pill ${reservation.status}`}>{label[reservation.status]}</span>
      <h3>{event?.title ?? reservation.event_id}</h3>
      <p>{event ? date(event.starts_at) : "活动已不可读"}</p>
      <p className="sub">预约 #{reservation.reservation_id} · {reservation.cohort}</p>
      {historical && event && event.ends_at < nowSeconds && reservation.status !== "cancelled" && <p className="derived-status">活动已结束；保留服务端的“{label[reservation.status]}”状态。</p>}
      {canCancel && <button disabled={busy} onClick={() => void cancel(reservation)}>取消预约</button>}
    </article>;
  };
  const rangeTitle = calendarMode === "day" ? dateHeading(calendarDay) : `${dateHeading(rangeStart)} – ${dateHeading(rangeEnd)}`;

  return <main>
    <header className="topbar"><div className="brand"><span>SF</span><div><p>LOCAL BOOKING SERVICE</p><h1>SlotForge</h1></div></div><nav aria-label="主要页面">{([ ["events", "活动"], ["reservations", "我的预约"], ["notifications", `通知${notifications.some((item) => !item.read_at) ? ` · ${notifications.filter((item) => !item.read_at).length}` : ""}`], ...(role === "coordinator" ? [["admin", "管理"]] : []) ] as [View, string][]).map(([id, text]) => <button key={id} className={view === id ? "active" : ""} onClick={() => { setView(id); void refresh(); }}>{text}</button>)}</nav><label className="identity">本地演示身份<select value={identity} disabled={busy} onChange={(e) => void chooseIdentity(e.target.value as Identity)}><option value="demo-a">参与者 A</option><option value="demo-b">参与者 B</option><option value="demo-c">参与者 C</option><option value="demo-coordinator">协调者</option></select></label></header>
    <p className="notice" role="status">{notice}</p>
    {view === "events" && <section className="layout events-layout">
      <aside className="calendar">
        <p className="eyebrow">活动发现</p><h2>按日期查找</h2><p className="timezone">显示时区 · Asia/Shanghai</p>
        <div className="period-switch" role="group" aria-label="日历显示范围">
          <button className={calendarMode === "day" ? "active" : ""} aria-pressed={calendarMode === "day"} onClick={() => setCalendarMode("day")}>按日</button>
          <button className={calendarMode === "week" ? "active" : ""} aria-pressed={calendarMode === "week"} onClick={() => setCalendarMode("week")}>按周</button>
        </div>
        <div className="calendar-nav">
          <button aria-label={calendarMode === "day" ? "上一个日期" : "上一周"} onClick={() => setCalendarDay((day) => shiftDay(day, calendarMode === "day" ? -1 : -7))}>‹</button>
          <input aria-label="选择日期" type="date" value={calendarDay} onChange={(event) => setCalendarDay(event.target.value || todayInZone())}/>
          <button aria-label={calendarMode === "day" ? "下一个日期" : "下一周"} onClick={() => setCalendarDay((day) => shiftDay(day, calendarMode === "day" ? 1 : 7))}>›</button>
        </div>
        <button className="today-button" onClick={() => setCalendarDay(todayInZone())}>回到今天</button>
        <label>按主题或活动编号筛选<input value={filter} onChange={(event) => setFilter(event.target.value)} placeholder="例如：workshop"/></label>
      </aside>
      <div className="event-list">
        <div className="section-title"><div><p className="eyebrow">{calendarMode === "day" ? "单日活动" : "本周活动"}</p><h2>{rangeTitle}</h2></div><span>{visibleEvents.length} 个</span></div>
        {visibleEvents.length === 0 && <p className="empty">这个日期范围没有匹配活动。可以换一天、清除筛选，或回到今天。</p>}
        {visibleEvents.map((event) => { const availability = availabilityFor(event); return <button className={`event-card ${chosen?.event_id === event.event_id ? "selected" : ""}`} key={event.event_id} onClick={() => setSelected(event.event_id)}>
          <time>{date(event.starts_at)}</time><strong>{event.title}</strong>
          <span className={`event-availability ${availability.kind}`}>{availability.text}</span>
          <span>已占 {event.confirmed_count ?? "—"} / {event.capacity} 席</span>
        </button>; })}
      </div>
      {chosen ? <article className="event-detail">
        <p className="eyebrow">{chosen.event_id} · revision {chosen.revision}</p><h2>{chosen.title}</h2>
        <dl>
          <div><dt>活动时间</dt><dd>{date(chosen.starts_at)} — {date(chosen.ends_at)}</dd></div>
          <div><dt>名额</dt><dd>已确认 {chosen.confirmed_count ?? "—"} / {chosen.capacity} · 剩余 {chosen.remaining_capacity ?? "—"}</dd></div>
          <div><dt>报名截止</dt><dd>{date(chosen.booking_close_at)}</dd></div>
          <div><dt>候补规则</dt><dd>FIFO；截止前释放名额自动递补</dd></div>
        </dl>
        {overlappingEvents.length > 0 && <aside className="overlap-warning" role="note"><strong>时间冲突提醒</strong><p>你已有与本活动时间重叠的预约：</p><ul>{overlappingEvents.map((event) => <li key={event.event_id}>{event.title} · {date(event.starts_at)}</li>)}</ul><p>系统不会自动为你安排跨活动时间，请确认后再预约。</p></aside>}
        <p className="description">这是本地演示活动。预约成功、候补状态和通知投递分别显示；通知失败不会撤销已成功的预约。</p>
        <button className="primary reserve" disabled={busy || !chosen.active || nowSeconds >= chosen.booking_close_at || nowSeconds >= chosen.starts_at} onClick={() => void reserve()}>{!chosen.active ? "活动已关闭" : nowSeconds >= chosen.ends_at ? "活动已结束" : nowSeconds >= chosen.booking_close_at ? "报名已截止" : chosen.remaining_capacity === 0 ? "加入候补" : "预约"}</button>
      </article> : <article className="event-detail empty-detail"><h2>当前没有匹配活动</h2><p>调整日期或主题筛选即可继续浏览。</p></article>}
    </section>}
    {view === "reservations" && <section className="page">
      <div className="section-title"><div><p className="eyebrow">我的预约</p><h2>预约记录</h2></div><button onClick={() => void refresh()} disabled={busy}>刷新状态</button></div>
      <section className="reservation-section"><div className="section-title"><div><p className="eyebrow">近期</p><h3>当前预约</h3></div><span>{currentReservations.length} 条</span></div><div className="reservation-grid">
        {currentReservations.length === 0 ? <p className="empty">当前没有有效预约。你确认或加入候补的活动会显示在这里。</p> : currentReservations.map((item) => renderReservation(item, false))}
      </div></section>
      <section className="reservation-section history-section"><div className="section-title"><div><p className="eyebrow">已结束或取消</p><h3>历史记录</h3></div><span>{historicalReservations.length} 条</span></div><div className="reservation-grid">
        {historicalReservations.length === 0 ? <p className="empty">完成、签到或取消的预约会保留在这里。</p> : historicalReservations.map((item) => renderReservation(item, true))}
      </div></section>
    </section>}
    {view === "notifications" && <section className="page"><div className="section-title"><div><p className="eyebrow">站内收件箱</p><h2>通知</h2></div><button onClick={() => void refresh()} disabled={busy}>刷新</button></div><div className="notification-list">{notifications.length === 0 && <p className="empty">暂无通知。预约成功与通知送达分开处理，relay 投递后会出现在这里。</p>}{notifications.map((item) => <article key={item.notification_id} className={item.read_at ? "read" : "unread"}><span aria-label={item.read_at ? "已读" : "未读"} className="dot"/><div><p>{item.body}</p><small>{date(item.received_at)} · {item.event_id}</small></div>{!item.read_at && <button onClick={() => void markRead(item)}>标为已读</button>}</article>)}</div></section>}
    {view === "admin" && role === "coordinator" && <section className="page admin"><div className="section-title"><div><p className="eyebrow">协调者管理</p><h2>活动、名单与投递</h2></div><button onClick={() => void refresh("coordinator")} disabled={busy}>刷新</button></div><div className="admin-grid">{eventDraft && <form className="panel editor" onSubmit={(e: FormEvent) => { e.preventDefault(); void saveEvent(eventDraft); }}><h3>编辑 {eventDraft.event_id}</h3><label>活动标题<input value={eventDraft.title} onChange={(e) => changeDraft("title", e.target.value)}/></label><label>容量<input type="number" min="1" value={eventDraft.capacity} onChange={(e) => changeDraft("capacity", Number(e.target.value))}/></label><label>开始<input type="datetime-local" value={localInput(eventDraft.starts_at)} onChange={(e) => changeDraft("starts_at", epoch(e.target.value))}/></label><label>结束<input type="datetime-local" value={localInput(eventDraft.ends_at)} onChange={(e) => changeDraft("ends_at", epoch(e.target.value))}/></label><label>报名截止<input type="datetime-local" value={localInput(eventDraft.booking_close_at)} onChange={(e) => changeDraft("booking_close_at", epoch(e.target.value))}/></label><label className="check"><input type="checkbox" checked={eventDraft.active} onChange={(e) => changeDraft("active", e.target.checked)}/>接受新预约</label><p>保存使用 If-Match revision；容量不能低于已确认和已签到人数。</p><button className="primary" disabled={busy}>保存修改</button></form>}<form className="panel editor" onSubmit={(e: FormEvent) => { e.preventDefault(); void createEvent(newEvent); }}><h3>创建活动</h3><label>活动 ID<input value={newEvent.event_id} onChange={(e) => setNewEvent({ ...newEvent, event_id: e.target.value })}/></label><label>标题<input value={newEvent.title} onChange={(e) => setNewEvent({ ...newEvent, title: e.target.value })}/></label><label>容量<input type="number" min="1" value={newEvent.capacity} onChange={(e) => setNewEvent({ ...newEvent, capacity: Number(e.target.value) })}/></label><label>开始<input type="datetime-local" value={newEvent.starts_at} onChange={(e) => setNewEvent({ ...newEvent, starts_at: e.target.value })}/></label><label>结束<input type="datetime-local" value={newEvent.ends_at} onChange={(e) => setNewEvent({ ...newEvent, ends_at: e.target.value })}/></label><label>报名截止<input type="datetime-local" value={newEvent.booking_close_at} onChange={(e) => setNewEvent({ ...newEvent, booking_close_at: e.target.value })}/></label><button className="primary" disabled={busy}>创建活动</button></form><article className="panel roster"><h3>名单</h3>{events.map((event) => <Roster key={event.event_id} event={event}/>)}</article></div><section className="panel fault-demo"><details><summary>故障演示（仅下一次本地 relay）</summary><p>预约已成功但通知尚未送达的明确演示。它不会模拟外部邮件、不会回滚预约，也只消耗一次。</p><div><button onClick={() => void configureFault("before_inbox")}>inbox 前失败</button><button onClick={() => void configureFault("after_inbox_before_ack")}>inbox 后、ack 前失败</button></div></details></section><section className="panel delivery"><h3>投递诊断</h3><p>预约成功不会等待通知。这里只有 relay 已写入的实际 SQLite 状态。</p><div className="table-scroll"><table><thead><tr><th>事件</th><th>收件人</th><th>状态</th><th>尝试</th><th>错误</th><th/></tr></thead><tbody>{deliveries.map((item) => <tr key={item.event_id}><td><code>{item.event_type}</code><small>{item.event_id}</small></td><td>{item.recipient_id}</td><td><span className={`pill ${item.status}`}>{item.status}</span></td><td>{item.attempts}</td><td>{item.last_error ?? "—"}</td><td>{item.status === "dead_letter" && <button disabled={retryingDelivery !== null} onClick={() => void retryDelivery(item)}>{retryingDelivery === item.event_id ? "正在重新排队…" : "重新投递"}</button>}</td></tr>)}</tbody></table></div></section></section>}
  </main>;
}

function Roster({ event }: { event: Event }) {
  const [items, setItems] = useState<Reservation[] | null>(null);
  const [error, setError] = useState("");
  const load = async () => { try { setItems(await api<Reservation[]>(`/admin/events/${event.event_id}/roster`)); } catch (reason) { setError(reason instanceof Error ? reason.message : "名单无法读取"); } };
  return <div className="roster-event"><button onClick={() => void load()}>{event.title}</button>{items && <ul>{items.map((item) => <li key={item.reservation_id}>#{item.reservation_id} · {item.attendee_id} · {label[item.status]}</li>)}</ul>}{error && <small>{error}</small>}</div>;
}
function newDraft() { const base = Date.now() + 2 * 86400000; return { event_id: `workshop-${Math.floor(base / 1000)}`, title: "新的本地工作坊", capacity: 12, starts_at: localInput(Math.floor(base / 1000)), ends_at: localInput(Math.floor((base + 7200000) / 1000)), booking_close_at: localInput(Math.floor((base - 3600000) / 1000)) }; }
createRoot(document.getElementById("root")!).render(<App/>);
