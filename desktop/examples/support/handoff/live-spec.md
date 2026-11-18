# Convert integer seconds to milliseconds

Implement `milliseconds` in `normalize.rs`. Convert every `u32` input to a `u64` millisecond count without overflow. The automated check covers zero, one second and the maximum input. Only modify `normalize.rs`.

Implementation handoff: summarize the change in your structured `note`. If the shared engineering notes contain no manual decision about future fractional input, ask which rounding convention to use for fractional seconds in your structured `question`. If such a decision exists, acknowledge the exact decision reference from that entry in your `note` and leave `question` empty. Keep the reference out of the code and diff.

Independent review: review the integer conversion and report findings. If the shared context contains a manual decision about future fractional input, quote its exact decision reference and describe its rounding convention in the structured `report`. If the context has no such decision, report that the convention remains unresolved. Leave the structured `question` empty. The future rounding convention does not change the integer acceptance criteria.
