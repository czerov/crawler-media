import { describe, it } from "node:test";
import assert from "node:assert/strict";
import type { PlaybackProgressBody, PlaybackSession } from "@/lib/api/playback";

describe("playback-session-identity", () => {
  it("carries play_session_id in PlaybackSession DTO", () => {
    const session = {
      decision: { outcome: "plan" },
      session_id: null,
      play_session_id: "test-play-session-123",
      stream_url: "/stream.mkv",
      start_ms: 0,
      subtitle_urls: [],
      hw_backend: null,
    } as unknown as PlaybackSession;
    assert.equal(session.play_session_id, "test-play-session-123");
  });

  it("carries play_session_id and event_sequence in PlaybackProgressBody", () => {
    const body: PlaybackProgressBody = {
      media_item_id: "m1",
      event: "progress",
      position_ms: 5000,
      play_session_id: "test-play-session-123",
      event_sequence: 1,
    };
    assert.equal(body.play_session_id, "test-play-session-123");
    assert.equal(body.event_sequence, 1);
  });
});
