# Convert seconds to milliseconds

Implement `milliseconds` in `normalize.rs`. It must convert every `u32` input to milliseconds and return a `u64` without overflow. The acceptance test covers zero, one second and the maximum input.

For the handoff, choose an identifier of the form `receipt-<six ASCII letters>` and include it in your structured `note`. Keep the identifier out of the code and diff.

For the independent review, check the conversion and acceptance coverage. In the structured `report`, quote the exact receipt identifier if the implementer's handoff supplied one. Report a missing receipt if you received none.
