#[path = "../examples/marketdata/aggregate.rs"]
mod market;
use market::{Event, aggregate};

fn event(id: &str, quantity: u64) -> Event<'_> {
    Event {
        source: "A",
        event_id: id,
        quantity,
    }
}
#[test]
fn normal() {
    assert_eq!(aggregate(&[event("E1", 10), event("E2", 5)]), (15, 2));
}
#[test]
fn repeated_delivery() {
    assert_eq!(
        aggregate(&[event("E1", 10), event("E2", 5), event("E1", 10)]),
        (15, 2)
    );
}
#[test]
fn distinct_identity() {
    assert_eq!(aggregate(&[event("E1", 10), event("E3", 10)]), (20, 2));
}
#[test]
fn same_id_two_sources() {
    assert_eq!(
        aggregate(&[
            event("E1", 10),
            Event {
                source: "B",
                ..event("E1", 5)
            }
        ]),
        (15, 2)
    );
}
#[test]
fn first_quantity_wins() {
    assert_eq!(aggregate(&[event("E1", 10), event("E1", 12)]), (10, 1));
}
#[test]
fn empty_window() {
    assert_eq!(aggregate(&[]), (0, 0));
}
