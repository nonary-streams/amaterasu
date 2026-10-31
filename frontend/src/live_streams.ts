namespace Amaterasu {
  export type LiveSample = NonNullable<HistoryDetailReply["detail"]>["samples"][number] & {
    interval_start_unix_ms: number;
    chart_source: "live";
  };
  type LiveDetail = NonNullable<HistoryDetailReply["detail"]>;
  interface LiveCounters { frames: bigint; bytes: bigint; gaps: bigint }
  interface LiveStream {
    session: HistorySessionSummary;
    counters: LiveCounters;
  }
  export interface LiveStreamSnapshot { epoch: string; sampledAt: bigint; sampledAtUnixMs: number; streams: LiveStream[] }
  interface LiveTrackerEntry {
    startedAt: number;
    sampledAt: bigint;
    sampledAtUnixMs: number;
    counters: LiveCounters;
    samples: LiveSample[];
  }

  function liveUnsigned(value: unknown): bigint {
    if (typeof value !== "string" || !/^(0|[1-9][0-9]{0,19})$/.test(value))
      throw new Error("Live stream counter is invalid.");
    const parsed = BigInt(value);
    if (parsed > 18_446_744_073_709_551_615n) throw new Error("Live stream counter is invalid.");
    return parsed;
  }

  function liveUnixMillis(value: unknown): number {
    if (typeof value !== "string" || !/^(0|[1-9][0-9]{0,15}|-[1-9][0-9]{0,15})$/.test(value))
      throw new Error("Live stream observation time is invalid.");
    const parsed = BigInt(value);
    if (parsed < -8_640_000_000_000_000n || parsed > 8_640_000_000_000_000n)
      throw new Error("Live stream observation time is invalid.");
    return Number(parsed);
  }

  export function parseLiveStreamSnapshot(value: unknown): LiveStreamSnapshot {
    if (!value || typeof value !== "object" || Array.isArray(value)) throw new Error("Live stream status is invalid.");
    const reply = value as Record<string, unknown>;
    if (reply.ok !== true || typeof reply.epoch !== "string" || !/^[0-9a-f]{32}$/.test(reply.epoch) ||
        !Array.isArray(reply.sessions) || reply.sessions.length > 2)
      throw new Error("Live stream status is invalid.");
    const sampledAt = liveUnsigned(reply.sampled_at_ms);
    const sampledAtUnixMs = liveUnixMillis(reply.sampled_at_unix_ms);
    const ids = new Set<string>();
    const streams = reply.sessions.map((value): LiveStream => {
      if (!value || typeof value !== "object" || Array.isArray(value)) throw new Error("Live stream status is invalid.");
      const row = value as Record<string, unknown>;
      const id = row.session_id;
      if (typeof id !== "string" || !id || new TextEncoder().encode(id).length > 128 || /[\u0000-\u001f\u007f-\u009f]/u.test(id) || ids.has(id) ||
          !Number.isSafeInteger(row.started_at_unix) ||
          (row.device_name !== null && (typeof row.device_name !== "string" || new TextEncoder().encode(row.device_name).length > 128 || /[\u0000-\u001f\u007f-\u009f]/u.test(row.device_name))) ||
          !["h264", "hevc", "av1"].includes(String(row.codec)) ||
          ["width", "height", "target_fps", "target_bitrate_kbps"].some((key) =>
            !Number.isSafeInteger(row[key]) || (row[key] as number) < 0 || (row[key] as number) > 0xffff_ffff))
        throw new Error("Live stream metadata is invalid.");
      ids.add(id);
      return {
        session: { session_id:id, started_at_unix:row.started_at_unix as number, ended_at_unix:null, duration_seconds:null,
          protocol:"sunshine-modern", client_name:"", device_name:(row.device_name as string | null) ?? "", app_name:"",
          width:row.width as number, height:row.height as number, target_fps:row.target_fps as number,
          target_bitrate_kbps:row.target_bitrate_kbps as number, codec:row.codec as string },
        counters: { frames:liveUnsigned(row.frames_sent), bytes:liveUnsigned(row.bytes_sent), gaps:liveUnsigned(row.sequence_gaps) },
      };
    });
    return { epoch:reply.epoch, sampledAt, sampledAtUnixMs, streams };
  }

  /** Browser-local rates are derived from successive host counter snapshots. */
  export class LiveStreamTracker {
    private readonly entries = new Map<string, LiveTrackerEntry>();
    private epoch: string | null = null;

    clear(): void { this.entries.clear(); this.epoch = null; }

    update(snapshot: LiveStreamSnapshot, retentionSeconds: number, maxPoints: number): Map<string, {detail: LiveDetail; rounded: boolean}> {
      if (snapshot.epoch !== this.epoch) { this.entries.clear(); this.epoch = snapshot.epoch; }
      const active = new Set(snapshot.streams.map((stream) => stream.session.session_id));
      for (const id of this.entries.keys()) if (!active.has(id)) this.entries.delete(id);
      const results = new Map<string, {detail: LiveDetail; rounded: boolean}>();
      for (const stream of snapshot.streams) {
        const id = stream.session.session_id;
        const old = this.entries.get(id);
        let samples = old?.samples ?? [];
        const monotonic = old && old.startedAt === stream.session.started_at_unix && snapshot.sampledAt >= old.sampledAt &&
          stream.counters.frames >= old.counters.frames && stream.counters.bytes >= old.counters.bytes && stream.counters.gaps >= old.counters.gaps;
        if (!monotonic) samples = [];
        else if (snapshot.sampledAt > old.sampledAt) {
          const elapsed = Number(snapshot.sampledAt - old.sampledAt);
          const wallElapsed = snapshot.sampledAtUnixMs - old.sampledAtUnixMs;
          if (elapsed > retentionSeconds * 1000 || wallElapsed <= 0 || Math.abs(wallElapsed - elapsed) > 1000) samples = [];
          else {
            samples = [...samples, {session_id:id, recorded_at_unix:snapshot.sampledAtUnixMs / 1000,
              interval_start_unix_ms:old.sampledAtUnixMs, chart_source:"live",
              frames_sent:Number(stream.counters.frames), bytes_sent:Number(stream.counters.bytes), dropped_frames:Number(stream.counters.gaps),
              actual_fps:Number(stream.counters.frames - old.counters.frames) * 1000 / elapsed,
              actual_bitrate_kbps:Number(stream.counters.bytes - old.counters.bytes) * 8 / elapsed,
              encode_latency_ms:null}];
          }
        }
        samples = samples.filter((sample) => sample.recorded_at_unix >= snapshot.sampledAtUnixMs / 1000 - retentionSeconds).slice(-maxPoints);
        if (!old || !monotonic || snapshot.sampledAt > old.sampledAt) {
          this.entries.set(id, {startedAt:stream.session.started_at_unix, sampledAt:snapshot.sampledAt,
            sampledAtUnixMs:snapshot.sampledAtUnixMs, counters:stream.counters, samples});
        } else old.samples = samples;
        const limit = BigInt(Number.MAX_SAFE_INTEGER);
        results.set(id, {detail:{session:stream.session, sample_count:samples.length, event_count:0,
          samples_truncated:false, events_truncated:false, samples, events:[]},
          rounded:stream.counters.frames > limit || stream.counters.bytes > limit || stream.counters.gaps > limit});
      }
      return results;
    }
  }
}
