"""Synthetic market-data fixture. All input events belong to a single window."""


def aggregate(events):
    """Return the total quantity and accepted event count for the window."""
    quantity = 0
    count = 0
    for event in events:
        quantity += event["quantity"]
        count += 1
    return {"quantity": quantity, "count": count}
