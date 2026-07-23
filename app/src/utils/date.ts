// Shared date helpers used by the Calendar view and the DatePicker component.
// All operate in local time (the app shows wall-clock dates; see the CalDAV
// timezone note in docs/ARCHITECTURE.md).

/** `Date` → `YYYY-MM-DD` (local). */
export function ymd(d: Date): string {
  return `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, "0")}-${String(d.getDate()).padStart(2, "0")}`;
}

/** First day of `d`'s month. */
export function monthStart(d: Date): Date {
  return new Date(d.getFullYear(), d.getMonth(), 1);
}

/** Parse `YYYY-MM-DD` into a local `Date`, or `null` when malformed/empty. */
export function parseYmd(s: string): Date | null {
  const [y, m, d] = (s ?? "").split("-").map(Number);
  if (!y || !m || !d) return null;
  return new Date(y, m - 1, d);
}

/** Parse `YYYY-MM-DD` or `YYYY-MM-DDTHH:MM[:SS]` into a local `Date`. */
export function parseDateTime(s: string): Date {
  const [datePart, timePart] = s.split("T");
  const [y, m, d] = datePart.split("-").map(Number);
  if (timePart) {
    const [hh, mm, ss] = timePart.split(":").map(Number);
    return new Date(y, m - 1, d, hh || 0, mm || 0, ss || 0);
  }
  return new Date(y, m - 1, d);
}

export function sameDay(a: Date, b: Date): boolean {
  return a.toDateString() === b.toDateString();
}

export function isToday(d: Date): boolean {
  return sameDay(d, new Date());
}

/** The 42 days (six Monday-based weeks) covering `cursor`'s month grid. */
export function monthGridDays(cursor: Date): Date[] {
  const first = monthStart(cursor);
  const offset = (first.getDay() + 6) % 7; // Monday-based
  const start = new Date(first);
  start.setDate(first.getDate() - offset);
  const out: Date[] = [];
  const c = new Date(start);
  for (let i = 0; i < 42; i++) {
    out.push(new Date(c));
    c.setDate(c.getDate() + 1);
  }
  return out;
}
