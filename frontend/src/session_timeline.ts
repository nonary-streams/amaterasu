namespace Amaterasu {
  type TimelineKind = "samples" | "events";
  type TimelineDetail = NonNullable<HistoryDetailReply["detail"]>;
  type TimelineCursor = { session_id: string; kind: TimelineKind; first_id: string; last_id: string; after_id: string | null; total: number };
  type TimelineWindow = { total: number; cursor: TimelineCursor | null };
  const TIMELINE_LIMITS = { samples: 25_000, events: 5_000 } as const;
  const TIMELINE_PAGE_LIMITS = { samples: 32, events: 8 } as const;

  function timelineRecord(value: unknown): Record<string, unknown> {
    if (!value || typeof value !== "object" || Array.isArray(value)) throw new Error("The retained history response is invalid.");
    return value as Record<string, unknown>;
  }

  function timelineId(value: unknown): string {
    if (typeof value !== "string" || !/^[1-9][0-9]{0,18}$/.test(value) || BigInt(value) > 9_223_372_036_854_775_807n)
      throw new Error("The retained history cursor is invalid.");
    return value;
  }

  function timelineCursor(value: unknown): TimelineCursor {
    const cursor = timelineRecord(value);
    if (Object.keys(cursor).sort().join(",") !== "after_id,first_id,kind,last_id,session_id,total" ||
        typeof cursor.session_id !== "string" || !cursor.session_id || (cursor.kind !== "samples" && cursor.kind !== "events") ||
        !Number.isSafeInteger(cursor.total) || (cursor.total as number) < 1)
      throw new Error("The retained history cursor is invalid.");
    const first = timelineId(cursor.first_id);
    const last = timelineId(cursor.last_id);
    const after = cursor.after_id === null ? null : timelineId(cursor.after_id);
    if (BigInt(first) > BigInt(last) || (after !== null && (BigInt(after) < BigInt(first) || BigInt(after) >= BigInt(last))))
      throw new Error("The retained history cursor is invalid.");
    return { session_id: cursor.session_id, kind: cursor.kind, first_id: first, last_id: last, after_id: after, total: cursor.total as number };
  }

  function timelineWindow(value: unknown, kind: TimelineKind, sessionId: string): TimelineWindow {
    const window = timelineRecord(value);
    if (!Number.isSafeInteger(window.total) || (window.total as number) < 0)
      throw new Error("The retained history count is invalid.");
    const total = window.total as number;
    if (total > TIMELINE_LIMITS[kind])
      throw new Error(`This session exceeds the browser limit of ${TIMELINE_LIMITS[kind]} retained ${kind}. The recent snapshot remains available.`);
    if (total === 0 && window.cursor === null) return { total, cursor: null };
    const cursor = timelineCursor(window.cursor);
    if (cursor.session_id !== sessionId || cursor.kind !== kind || cursor.total !== total || cursor.after_id !== null) throw new Error("The retained history window is inconsistent.");
    return { total, cursor };
  }

  export interface LiveSampleSnapshot {
    session: HistorySessionSummary;
    ids: string[];
    samples: TimelineDetail["samples"];
  }

  /** Load a bounded recent window, reusing rows only when the new window accounts for them. */
  export async function loadLiveSampleSnapshot(
    api: Pick<Api, "post">, sessionId: string, limit: number,
    previous: LiveSampleSnapshot | null, active: () => boolean,
  ): Promise<LiveSampleSnapshot | null> {
    const opened = timelineRecord(await api.post<unknown>("history/timeline_open", { session_id: sessionId, sample_limit: limit }));
    if (!active()) return null;
    if (opened.ok !== true) throw new Error("Live sample history is unavailable.");
    const session = Amaterasu.parseHistoryDetailReply({ok:true,detail:{session:opened.session,
      sample_count:0,event_count:0,samples_truncated:false,events_truncated:false,samples:[],events:[]}}, sessionId).detail!.session;
    const window = timelineWindow(opened.samples, "samples", sessionId);
    if (window.total > limit) throw new Error("The live sample window exceeds its limit.");
    if (!window.cursor) return { session, ids: [], samples: [] };
    const boundary = window.cursor;
    let prefix: LiveSampleSnapshot = { session, ids: [], samples: [] };
    if (previous && previous.ids.length === previous.samples.length && previous.session.session_id === sessionId) {
      const keep = previous.ids.map((id, index) => ({id, sample:previous.samples[index]!})).filter(({id}) =>
        BigInt(id) >= BigInt(boundary.first_id) && BigInt(id) <= BigInt(boundary.last_id));
      if (keep[0]?.id === boundary.first_id && keep.length <= window.total &&
          keep.every(({id}, index) => index === 0 || BigInt(id) > BigInt(keep[index - 1]!.id)))
        prefix = { session, ids:keep.map(({id}) => id), samples:keep.map(({sample}) => sample) };
    }

    async function read(reuse: LiveSampleSnapshot): Promise<LiveSampleSnapshot | null> {
      const result: LiveSampleSnapshot = { session, ids:[...reuse.ids], samples:[...reuse.samples] };
      const lastKnown = result.ids.at(-1);
      if (lastKnown === boundary.last_id) return result.ids.length === window.total ? result : null;
      let cursor: TimelineCursor | null = { ...boundary, after_id:lastKnown ?? null };
      while (cursor && active()) {
        const previousCursor: TimelineCursor = cursor;
        const reply = timelineRecord(await api.post<unknown>("history/timeline_page", {
          session_id:sessionId, kind:"samples", cursor, limit:TIMELINE_PAGE_LIMITS.samples,
        }));
        if (!active()) return null;
        if (reply.ok !== true || reply.session_id !== sessionId || reply.kind !== "samples" || reply.total !== window.total ||
            !Array.isArray(reply.entries) || !Array.isArray(reply.entry_ids) || reply.entries.length === 0 ||
            reply.entries.length > TIMELINE_PAGE_LIMITS.samples || reply.entry_ids.length !== reply.entries.length)
          throw new Error("The live sample page is invalid.");
        const ids = reply.entry_ids.map(timelineId);
        let last = previousCursor.after_id;
        for (const id of ids) {
          if (BigInt(id) < BigInt(boundary.first_id) || BigInt(id) > BigInt(boundary.last_id) ||
              (last !== null && BigInt(id) <= BigInt(last))) throw new Error("The live sample IDs are invalid.");
          last = id;
        }
        const parsed = Amaterasu.parseHistoryDetailReply({ok:true,detail:{session,
          sample_count:reply.entries.length,event_count:0,samples_truncated:false,events_truncated:false,
          samples:reply.entries,events:[]}}, sessionId).detail!;
        result.ids.push(...ids);
        result.samples.push(...parsed.samples);
        if (result.ids.length > window.total) return null;
        if (reply.next_cursor === null) {
          if (last !== boundary.last_id) throw new Error("The live sample page ended early.");
          cursor = null;
        } else {
          const next = timelineCursor(reply.next_cursor);
          if (next.session_id !== sessionId || next.kind !== "samples" || next.first_id !== boundary.first_id ||
              next.last_id !== boundary.last_id || next.total !== boundary.total || next.after_id !== last)
            throw new Error("The live sample cursor is invalid.");
          cursor = next;
        }
      }
      if (!active()) return null;
      return result.ids.length === window.total ? result : null;
    }
    let result: LiveSampleSnapshot | null;
    try {
      result = await read(prefix);
    } catch (error) {
      if (!prefix.ids.length || !error || typeof error !== "object" || (error as {code?: unknown}).code !== "invalid_request") throw error;
      result = null;
    }
    if (!active()) return null;
    if (!result && prefix.ids.length) result = await read({ session, ids:[], samples:[] });
    if (!active()) return null;
    if (!result) throw new Error("The live sample window changed while it was loading.");
    return result;
  }

  /** Load one immutable row range per kind; no partial result is returned on failure. */
  export async function loadRetainedTimeline(
    api: Pick<Api, "post">,
    sessionId: string,
    active: () => boolean,
    progress: (samples: number, sampleTotal: number, events: number, eventTotal: number) => void,
  ): Promise<TimelineDetail | null> {
    let halted = false;
    const current = () => !halted && active();
    const opened = timelineRecord(await api.post<unknown>("history/timeline_open", { session_id: sessionId }));
    if (!current()) return null;
    if (opened.ok !== true) throw new Error("Retained history is unavailable.");
    const session = timelineRecord(opened.session);
    const sampleWindow = timelineWindow(opened.samples, "samples", sessionId);
    const eventWindow = timelineWindow(opened.events, "events", sessionId);
    // Reuse the strict detail parser for the session metadata and each bounded page.
    const metadata = Amaterasu.parseHistoryDetailReply({ok:true,detail:{session,
      sample_count:0,event_count:0,samples_truncated:false,events_truncated:false,samples:[],events:[]}}, sessionId).detail!.session;
    const counts = { samples: 0, events: 0 };
    const report = () => { if (current()) progress(counts.samples, sampleWindow.total, counts.events, eventWindow.total); };
    async function read(kind: TimelineKind, window: TimelineWindow): Promise<TimelineDetail[typeof kind]> {
      let cursor = window.cursor;
      const rows: unknown[] = [];
      while (cursor && current()) {
        const previous = cursor;
        const reply = timelineRecord(await api.post<unknown>("history/timeline_page", {
          session_id: sessionId, kind, cursor, limit: TIMELINE_PAGE_LIMITS[kind],
        }));
        if (!current()) break;
        if (reply.ok !== true || reply.session_id !== sessionId || reply.kind !== kind || reply.total !== window.total ||
            !Array.isArray(reply.entries) || reply.entries.length === 0 || reply.entries.length > TIMELINE_PAGE_LIMITS[kind])
          throw new Error("The retained history page is invalid.");
        const entries = reply.entries;
        Amaterasu.parseHistoryDetailReply({ok:true,detail:{session:metadata,
          sample_count:kind === "samples" ? entries.length : 0, event_count:kind === "events" ? entries.length : 0,
          samples_truncated:false,events_truncated:false,samples:kind === "samples" ? entries : [],events:kind === "events" ? entries : []}}, sessionId);
        rows.push(...entries);
        if (rows.length > window.total) throw new Error("The retained history page exceeds its snapshot.");
        counts[kind] = rows.length;
        report();
        if (reply.next_cursor === null) cursor = null;
        else {
          const next = timelineCursor(reply.next_cursor);
          if (next.session_id !== previous.session_id || next.kind !== previous.kind || next.first_id !== previous.first_id || next.last_id !== previous.last_id || next.total !== previous.total ||
              next.after_id === null || (previous.after_id !== null && BigInt(next.after_id) <= BigInt(previous.after_id)))
            throw new Error("The retained history cursor did not advance.");
          cursor = next;
        }
        if ((cursor === null) !== (rows.length === window.total))
          throw new Error("The retained history page does not match its snapshot count.");
      }
      return rows as TimelineDetail[typeof kind];
    }
    report();
    try {
      const [samples, events] = await Promise.all([read("samples", sampleWindow), read("events", eventWindow)]);
      if (!current()) return null;
      return { session: metadata, sample_count: sampleWindow.total, event_count: eventWindow.total,
        samples_truncated:false,events_truncated:false,
        samples:samples as TimelineDetail["samples"],events:events as TimelineDetail["events"] };
    } catch (error) {
      halted = true;
      if (!active()) return null;
      throw error;
    }
  }
}
