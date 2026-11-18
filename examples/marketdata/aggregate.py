"""Synthetic market-data fixture. All input events belong to a single window."""


def aggregate(events):
    """Return the total quantity and accepted event count for the window."""
    quantity = 0
    seen = set()
    for event in events:
        identity = (event["source"], event["event_id"])
        if identity in seen:
            continue
        seen.add(identity)
        quantity += event["quantity"]
    return {"quantity": quantity, "count": len(seen)}
