use std::collections::HashSet;

pub struct Event<'a> {
    pub source: &'a str,
    pub event_id: &'a str,
    pub quantity: u64,
}

/// Count the first occurrence of each identity in a single window.
pub fn aggregate(events: &[Event<'_>]) -> (u128, usize) {
    let mut seen = HashSet::new();
    let mut total = 0;
    for event in events {
        if seen.insert((event.source, event.event_id)) {
            total += u128::from(event.quantity);
        }
    }
    (total, seen.len())
}
