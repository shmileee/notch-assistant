//! The island's open/closed behaviour as one state machine (Rune's `rune::machine` pattern).
//!
//! Hovering the notch arms a short delay before opening, so sweeping the pointer across the
//! menu bar doesn't flash the panel. Leaving the open panel arms a longer delay before it
//! closes, so a pointer that slips off the edge for a moment can come back. A click opens at
//! once. Timers are events: the state holds each timer's `Attempt` and due time, and `poll`
//! feeds `Due` back in.

use rune::machine::{Attempt, Effects, Seq};

pub const OPEN_DELAY: f64 = 0.12;
pub const CLOSE_DELAY: f64 = 0.40;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum Island {
    #[default]
    Collapsed,
    Arming { attempt: Attempt, due: f64 },
    Open,
    Closing { attempt: Attempt, due: f64 },
}

#[derive(Clone, Copy, Debug)]
pub enum Event {
    Enter { now: f64 },
    Leave { now: f64 },
    Press,
    Due { now: f64 },
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Effect {
    WakeAt(f64),
}

impl Island {
    /// Whether the panel is drawn expanded (it stays expanded while a close is pending).
    pub fn is_open(self) -> bool { matches!(self, Island::Open | Island::Closing { .. }) }

    /// The pending timer, if any.
    pub fn due(self) -> Option<(Attempt, f64)> {
        match self {
            Island::Arming { attempt, due } | Island::Closing { attempt, due } => Some((attempt, due)),
            _ => None,
        }
    }
}

pub fn step(seq: &mut Seq, island: &mut Island, event: Event) -> Effects<Effect> {
    match (*island, event) {
        (Island::Collapsed, Event::Enter { now }) => {
            let (attempt, due) = (seq.next(), now + OPEN_DELAY);
            *island = Island::Arming { attempt, due };
            Effects::one(Effect::WakeAt(due))
        }
        (Island::Arming { .. }, Event::Leave { .. }) => {
            *island = Island::Collapsed;
            Effects::none()
        }
        (Island::Collapsed | Island::Arming { .. } | Island::Closing { .. }, Event::Press) => {
            *island = Island::Open;
            Effects::none()
        }
        (Island::Open, Event::Leave { now }) => {
            let (attempt, due) = (seq.next(), now + CLOSE_DELAY);
            *island = Island::Closing { attempt, due };
            Effects::one(Effect::WakeAt(due))
        }
        (Island::Closing { .. }, Event::Enter { .. }) => {
            *island = Island::Open;
            Effects::none()
        }
        (Island::Arming { due, .. }, Event::Due { now }) if now >= due => {
            *island = Island::Open;
            Effects::none()
        }
        (Island::Closing { due, .. }, Event::Due { now }) if now >= due => {
            *island = Island::Collapsed;
            Effects::none()
        }
        _ => Effects::none(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(island: &mut Island, seq: &mut Seq, events: &[Event]) {
        for &e in events { let _ = step(seq, island, e); }
    }

    #[test]
    fn hover_opens_after_the_delay_and_a_quick_pass_does_not() {
        let (mut seq, mut island) = (Seq::default(), Island::default());
        run(&mut island, &mut seq, &[Event::Enter { now: 0.0 }, Event::Due { now: 0.05 }]);
        assert!(!island.is_open(), "too early");
        run(&mut island, &mut seq, &[Event::Due { now: OPEN_DELAY }]);
        assert_eq!(island, Island::Open);

        let (mut seq, mut island) = (Seq::default(), Island::default());
        run(&mut island, &mut seq, &[Event::Enter { now: 0.0 }, Event::Leave { now: 0.05 }, Event::Due { now: 1.0 }]);
        assert_eq!(island, Island::Collapsed, "a pointer sweeping past never opens it");
    }

    #[test]
    fn leaving_closes_after_a_grace_period_and_coming_back_keeps_it_open() {
        let (mut seq, mut island) = (Seq::default(), Island::Open);
        run(&mut island, &mut seq, &[Event::Leave { now: 1.0 }, Event::Enter { now: 1.2 }, Event::Due { now: 2.0 }]);
        assert_eq!(island, Island::Open);
        run(&mut island, &mut seq, &[Event::Leave { now: 3.0 }, Event::Due { now: 3.0 + CLOSE_DELAY }]);
        assert_eq!(island, Island::Collapsed);
    }

    #[test]
    fn a_click_opens_at_once() {
        let (mut seq, mut island) = (Seq::default(), Island::default());
        run(&mut island, &mut seq, &[Event::Press]);
        assert_eq!(island, Island::Open);
    }
}
