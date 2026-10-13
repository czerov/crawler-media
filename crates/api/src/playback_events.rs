//! Shared playback event classification, generation isolation, and validation.

use domain::LedgerId;
use store::{PlaySessionBinding, PlaySessionIdState, SessionRow};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PlaybackEventIdentity {
    pub play_session_id: Option<String>,
    pub event_sequence: Option<i64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlaybackEventKind {
    Start,
    Progress,
    Stop,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EventDecision {
    Start,
    Update,
    Stop,
    Ignore,
}

/// Classify an incoming playback event based on session identity, bindings, and previous session.
///
/// Implements the state machine in Section 2.3 and 2.4 of Batch 2 design:
/// - With PlaySessionId:
///   * Verified against server-issued binding (user, device, ledger).
///   * Start / Playing can activate a pending session and close older ones.
///   * Progress / Stop only updates an active session matching the exact session ID.
///   * EventSequence if present must be strictly greater than last_event_sequence.
///   * Unknown, closed, or mismatched ID events are ignored (return Ignore).
/// - Without PlaySessionId (Legacy client):
///   * Start creates/replaces legacy session for matching or new target.
///   * Progress / Stop only allowed if current active session matches the exact LedgerId.
///     If target differs or no current session, returns Ignore.
pub fn classify_event(
    previous: Option<&SessionRow>,
    binding: Option<&PlaySessionBinding>,
    identity: &PlaybackEventIdentity,
    ledger: LedgerId,
    event: PlaybackEventKind,
) -> EventDecision {
    if let Some(session_id) = &identity.play_session_id {
        // ID-based playback session
        let Some(b) = binding else {
            // Unrecognized or forged session ID -> Ignore
            return EventDecision::Ignore;
        };

        // Binding must match target ledger
        if b.ledger_id != ledger {
            return EventDecision::Ignore;
        }

        // Sequence check if provided
        if let (Some(prev), Some(seq)) = (previous, identity.event_sequence) {
            if let Some(prev_seq) = prev.last_event_sequence {
                if seq <= prev_seq {
                    return EventDecision::Ignore;
                }
            }
        }

        match event {
            PlaybackEventKind::Start => {
                match b.state {
                    PlaySessionIdState::Pending | PlaySessionIdState::Active => {
                        EventDecision::Start
                    }
                    PlaySessionIdState::Closed => EventDecision::Ignore,
                }
            }
            PlaybackEventKind::Progress => {
                match b.state {
                    PlaySessionIdState::Active => {
                        // Must match current active session row's play_session_id
                        if previous.and_then(|p| p.play_session_id.as_deref()) == Some(session_id.as_str()) {
                            EventDecision::Update
                        } else {
                            EventDecision::Ignore
                        }
                    }
                    _ => EventDecision::Ignore,
                }
            }
            PlaybackEventKind::Stop => {
                match b.state {
                    PlaySessionIdState::Active => {
                        if previous.and_then(|p| p.play_session_id.as_deref()) == Some(session_id.as_str()) {
                            EventDecision::Stop
                        } else {
                            EventDecision::Ignore
                        }
                    }
                    _ => EventDecision::Ignore,
                }
            }
        }
    } else {
        // Legacy no-ID playback session
        match event {
            PlaybackEventKind::Start => EventDecision::Start,
            PlaybackEventKind::Progress => {
                if let Some(prev) = previous {
                    let cur_ledger_match = prev.ledger_id.as_deref() == Some(&ledger.to_string());
                    if cur_ledger_match {
                        EventDecision::Update
                    } else {
                        EventDecision::Ignore
                    }
                } else {
                    EventDecision::Ignore
                }
            }
            PlaybackEventKind::Stop => {
                if let Some(prev) = previous {
                    let cur_ledger_match = prev.ledger_id.as_deref() == Some(&ledger.to_string());
                    if cur_ledger_match {
                        EventDecision::Stop
                    } else {
                        EventDecision::Ignore
                    }
                } else {
                    EventDecision::Ignore
                }
            }
        }
    }
}
