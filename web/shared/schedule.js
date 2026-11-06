// Shared schedule engine — used by the player (offline evaluation) and the dashboard (previews).
// All times are display-local wall-clock times.

const pad = (n) => String(n).padStart(2, "0");

export function localStamp(d = new Date()) {
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}T${pad(d.getHours())}:${pad(d.getMinutes())}`;
}

export function hhmm(d = new Date()) {
  return `${pad(d.getHours())}:${pad(d.getMinutes())}`;
}

/** bit0 = Monday … bit6 = Sunday */
export function dayBit(d = new Date()) {
  return 1 << ((d.getDay() + 6) % 7);
}

/** Daily window check that supports overnight windows such as 22:00 → 06:00. */
export function inWindow(from, to, now = new Date()) {
  if (!from || !to) return true;
  const t = hhmm(now);
  if (from === to) return true;
  return from < to ? t >= from && t < to : t >= from || t < to;
}

/** Does a recurring rule ({days_mask, time_from, time_to}) match right now? */
export function ruleMatches(rule, now = new Date()) {
  if (!rule) return true;
  const mask = rule.days_mask ?? 127;
  // For overnight windows the early-morning part belongs to the previous day's rule.
  let dayRef = now;
  if (rule.time_from && rule.time_to && rule.time_from > rule.time_to && hhmm(now) < rule.time_to) {
    dayRef = new Date(now.getTime() - 86400000);
  }
  if (!(mask & dayBit(dayRef))) return false;
  return inWindow(rule.time_from, rule.time_to, now);
}

export function scheduleActive(s, now = new Date()) {
  if (!s.enabled) return false;
  const stamp = localStamp(now);
  if (s.starts_at && stamp < s.starts_at) return false;
  if (s.ends_at && stamp >= s.ends_at) return false;
  return ruleMatches(s, now);
}

/**
 * Returns the layout ids that should be on screen now.
 * Highest priority wins; equal-priority schedules rotate (one layout after another).
 */
export function activeLayouts(manifest, now = new Date()) {
  const live = (manifest.schedules || []).filter(
    (s) => scheduleActive(s, now) && manifest.layouts && manifest.layouts[s.layout_id]
  );
  if (live.length) {
    const top = Math.max(...live.map((s) => s.priority));
    const ids = [...new Set(live.filter((s) => s.priority === top).map((s) => s.layout_id))];
    return { layouts: ids, source: "schedule", priority: top };
  }
  if (manifest.default_layout_id && manifest.layouts?.[manifest.default_layout_id]) {
    return { layouts: [manifest.default_layout_id], source: "default", priority: -1 };
  }
  return { layouts: [], source: "none", priority: -1 };
}

/** Item-level day-parting: items with a `when` rule only play inside it. */
export function itemPlayable(item, now = new Date()) {
  if (!item.when) return true;
  const w = item.when;
  if (w.valid_from && localStamp(now) < w.valid_from) return false;
  if (w.valid_to && localStamp(now) >= w.valid_to) return false;
  return ruleMatches({ days_mask: w.days_mask ?? 127, time_from: w.time_from || null, time_to: w.time_to || null }, now);
}

/** Replace {{key}} placeholders with values from a data object (trigger payloads, feeds). */
export function fillTemplate(str, data) {
  if (!str || !data) return str || "";
  return String(str).replace(/\{\{\s*([\w.]+)\s*\}\}/g, (_, k) => {
    const v = k.split(".").reduce((o, p) => (o == null ? o : o[p]), data);
    return v == null ? "" : String(v);
  });
}
