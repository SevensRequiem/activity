use serde::{Deserialize, Serialize};

use crate::classify::State;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Hold {
    Never,
    WhenBusy,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Policy {
    pub hold: Hold,
    pub busy: Vec<State>,
    /// Deliver anyway after being held this long.
    pub max_defer_min: u32,
    /// Re-ping an unacknowledged reminder this often; 0 pings once.
    pub repeat_every_min: u32,
    pub max_repeats: u32,
    /// From this delivery count on, also ping the phone; 0 never escalates.
    pub escalate_after: u32,
    /// Ring the phone on the final repeat.
    pub ring_phone: bool,
    /// While away (asleep, locked), wait instead of going to the phone, and
    /// deliver on return.
    pub pause_when_away: bool,
    /// Never expire. After the last repeat it stays open, quietly, until
    /// resolved. Medicine needs this: a missed dose is still owed.
    pub keep_open: bool,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            hold: Hold::WhenBusy,
            busy: vec![State::Gaming, State::Watching, State::Focus],
            max_defer_min: 120,
            repeat_every_min: 10,
            max_repeats: 3,
            escalate_after: 3,
            ring_phone: false,
            pause_when_away: false,
            keep_open: false,
        }
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum Error {
    #[error("max_defer_min must be at most 1440")]
    Defer,
    #[error("repeat_every_min must be at most 1440")]
    Repeat,
    #[error("max_repeats must be at most 50")]
    Repeats,
}

impl Policy {
    pub fn validate(&self) -> Result<(), Error> {
        if self.max_defer_min > 1440 {
            return Err(Error::Defer);
        }
        if self.repeat_every_min > 1440 {
            return Err(Error::Repeat);
        }
        if self.max_repeats > 50 {
            return Err(Error::Repeats);
        }
        Ok(())
    }
}

/// Where an occurrence is in its life. Times are unix seconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "phase", rename_all = "snake_case")]
pub enum Phase {
    Pending,
    Deferred { since: i64 },
    Delivered { count: u32, last: i64 },
    Acked,
    Skipped,
    Expired,
}

impl Phase {
    pub fn is_open(self) -> bool {
        matches!(
            self,
            Phase::Pending | Phase::Deferred { .. } | Phase::Delivered { .. }
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Delivery {
    pub phone: bool,
    pub ring: bool,
    pub count: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Step {
    pub phase: Phase,
    pub deliver: Option<Delivery>,
    /// When this occurrence next needs a look; `None` waits for an outside event.
    pub wake: Option<i64>,
}

/// Advance one occurrence given the user's current state. Pure, so the whole
/// hold/forward/escalate behavior is tested without a desktop.
pub fn step(due: i64, phase: Phase, policy: &Policy, state: State, now: i64, grace: i64) -> Step {
    let busy = policy.hold == Hold::WhenBusy && policy.busy.contains(&state);
    let away = state == State::Away;
    let paused = policy.pause_when_away && away;
    let mins = |m: u32| i64::from(m) * 60;
    let stay = |wake| Step {
        phase,
        deliver: None,
        wake,
    };
    let deliver = |count: u32| Step {
        phase: Phase::Delivered { count, last: now },
        deliver: Some(Delivery {
            phone: away || (policy.escalate_after > 0 && count >= policy.escalate_after),
            ring: policy.ring_phone && count > 1 && count > policy.max_repeats,
            count,
        }),
        wake: (policy.repeat_every_min > 0 && count <= policy.max_repeats)
            .then(|| now + mins(policy.repeat_every_min)),
    };
    match phase {
        Phase::Pending if now < due => stay(Some(due)),
        Phase::Pending if now > due + grace && !policy.keep_open => Step {
            phase: Phase::Expired,
            deliver: None,
            wake: None,
        },
        Phase::Pending if paused => Step {
            phase: Phase::Deferred { since: now },
            deliver: None,
            wake: None,
        },
        Phase::Pending if busy && !away => Step {
            phase: Phase::Deferred { since: now },
            deliver: None,
            wake: Some(now + mins(policy.max_defer_min)),
        },
        Phase::Pending => deliver(1),
        Phase::Deferred { since } => {
            if paused {
                return stay(None);
            }
            let limit = since + mins(policy.max_defer_min);
            if busy && !away && now < limit {
                stay(Some(limit))
            } else {
                deliver(1)
            }
        }
        Phase::Delivered { count, last } => {
            if policy.repeat_every_min == 0 {
                return stay(None);
            }
            let next = last + mins(policy.repeat_every_min);
            if now < next {
                return stay(Some(next));
            }
            if paused {
                return stay(None);
            }
            if count > policy.max_repeats && policy.keep_open {
                return stay(None);
            }
            if count > policy.max_repeats {
                return Step {
                    phase: Phase::Expired,
                    deliver: None,
                    wake: None,
                };
            }
            // A held reminder keeps quiet while busy, up to max_defer past the repeat time.
            let limit = next + mins(policy.max_defer_min);
            if busy && !away && now < limit {
                return stay(Some(limit));
            }
            deliver(count + 1)
        }
        Phase::Acked | Phase::Skipped | Phase::Expired => stay(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GRACE: i64 = 1800;

    /// name, phase, policy, state, now, then the expected phase, delivery and wake.
    type Case<'a> = (
        &'a str,
        Phase,
        &'a Policy,
        State,
        i64,
        Phase,
        Option<Delivery>,
        Option<i64>,
    );

    #[test]
    fn step_cases() {
        let held = Policy::default();
        let urgent = Policy {
            hold: Hold::Never,
            ..Policy::default()
        };
        let ringing = Policy {
            ring_phone: true,
            max_repeats: 2,
            ..Policy::default()
        };
        let once = Policy {
            repeat_every_min: 0,
            ..Policy::default()
        };
        let meds = Policy {
            hold: Hold::Never,
            pause_when_away: true,
            keep_open: true,
            ..Policy::default()
        };
        let d = |phone, ring, count| Some(Delivery { phone, ring, count });
        #[rustfmt::skip]
        let cases: &[Case] = &[
            ("not yet due", Phase::Pending, &held, State::Browsing, 900, Phase::Pending, None, Some(1000)),
            ("due and free", Phase::Pending, &held, State::Browsing, 1000, Phase::Delivered { count: 1, last: 1000 }, d(false, false, 1), Some(1600)),
            ("due while gaming is held", Phase::Pending, &held, State::Gaming, 1000, Phase::Deferred { since: 1000 }, None, Some(1000 + 7200)),
            ("hold never fires while gaming", Phase::Pending, &urgent, State::Gaming, 1000, Phase::Delivered { count: 1, last: 1000 }, d(false, false, 1), Some(1600)),
            ("away goes to phone", Phase::Pending, &held, State::Away, 1000, Phase::Delivered { count: 1, last: 1000 }, d(true, false, 1), Some(1600)),
            ("missed past grace expires", Phase::Pending, &held, State::Browsing, 1000 + GRACE + 1, Phase::Expired, None, None),
            ("missed at grace edge still fires", Phase::Pending, &held, State::Browsing, 1000 + GRACE, Phase::Delivered { count: 1, last: 1000 + GRACE }, d(false, false, 1), Some(1000 + GRACE + 600)),
            ("deferred still busy", Phase::Deferred { since: 1000 }, &held, State::Watching, 2000, Phase::Deferred { since: 1000 }, None, Some(8200)),
            ("deferred freed delivers", Phase::Deferred { since: 1000 }, &held, State::Chatting, 2000, Phase::Delivered { count: 1, last: 2000 }, d(false, false, 1), Some(2600)),
            ("deferred hits max_defer", Phase::Deferred { since: 1000 }, &held, State::Gaming, 8200, Phase::Delivered { count: 1, last: 8200 }, d(false, false, 1), Some(8800)),
            ("repeat not yet due", Phase::Delivered { count: 1, last: 1000 }, &held, State::Browsing, 1500, Phase::Delivered { count: 1, last: 1000 }, None, Some(1600)),
            ("repeat due", Phase::Delivered { count: 1, last: 1000 }, &held, State::Browsing, 1600, Phase::Delivered { count: 2, last: 1600 }, d(false, false, 2), Some(2200)),
            ("repeat escalates to phone", Phase::Delivered { count: 2, last: 1000 }, &held, State::Browsing, 1600, Phase::Delivered { count: 3, last: 1600 }, d(true, false, 3), Some(2200)),
            ("final repeat has no further wake", Phase::Delivered { count: 3, last: 1000 }, &held, State::Browsing, 1600, Phase::Delivered { count: 4, last: 1600 }, d(true, false, 4), None),
            ("repeats exhausted expire", Phase::Delivered { count: 4, last: 1000 }, &held, State::Browsing, 1600, Phase::Expired, None, None),
            ("repeat held while busy", Phase::Delivered { count: 1, last: 1000 }, &held, State::Focus, 1600, Phase::Delivered { count: 1, last: 1000 }, None, Some(1600 + 7200)),
            ("final repeat rings", Phase::Delivered { count: 2, last: 1000 }, &ringing, State::Browsing, 1600, Phase::Delivered { count: 3, last: 1600 }, d(true, true, 3), None),
            ("no repeats waits for ack", Phase::Delivered { count: 1, last: 1000 }, &once, State::Browsing, 99_999, Phase::Delivered { count: 1, last: 1000 }, None, None),
            ("dose due while asleep waits", Phase::Pending, &meds, State::Away, 1000, Phase::Deferred { since: 1000 }, None, None),
            ("dose still waits hours later", Phase::Deferred { since: 1000 }, &meds, State::Away, 30_000, Phase::Deferred { since: 1000 }, None, None),
            ("dose delivers on waking", Phase::Deferred { since: 1000 }, &meds, State::Active, 30_000, Phase::Delivered { count: 1, last: 30_000 }, d(false, false, 1), Some(30_600)),
            ("dose fires while gaming", Phase::Pending, &meds, State::Gaming, 1000, Phase::Delivered { count: 1, last: 1000 }, d(false, false, 1), Some(1600)),
            ("missed dose never expires", Phase::Pending, &meds, State::Browsing, 1000 + GRACE * 10, Phase::Delivered { count: 1, last: 1000 + GRACE * 10 }, d(false, false, 1), Some(1000 + GRACE * 10 + 600)),
            ("repeat waits while asleep", Phase::Delivered { count: 1, last: 1000 }, &meds, State::Away, 1600, Phase::Delivered { count: 1, last: 1000 }, None, None),
            ("dose stays open after repeats", Phase::Delivered { count: 4, last: 1000 }, &meds, State::Browsing, 1600, Phase::Delivered { count: 4, last: 1000 }, None, None),
            ("acked is terminal", Phase::Acked, &held, State::Browsing, 5000, Phase::Acked, None, None),
        ];
        for (name, phase, policy, state, now, want_phase, want_deliver, want_wake) in cases {
            let got = step(1000, *phase, policy, *state, *now, GRACE);
            assert_eq!(got.phase, *want_phase, "case {name}: phase");
            assert_eq!(got.deliver, *want_deliver, "case {name}: delivery");
            assert_eq!(got.wake, *want_wake, "case {name}: wake");
        }
    }

    #[test]
    fn validate_bounds() {
        let at = |defer, repeat, repeats| Policy {
            max_defer_min: defer,
            repeat_every_min: repeat,
            max_repeats: repeats,
            ..Policy::default()
        };
        assert_eq!(at(1440, 1440, 50).validate(), Ok(()));
        assert_eq!(at(1441, 0, 0).validate(), Err(Error::Defer));
        assert_eq!(at(0, 1441, 0).validate(), Err(Error::Repeat));
        assert_eq!(at(0, 0, 51).validate(), Err(Error::Repeats));
    }
}
