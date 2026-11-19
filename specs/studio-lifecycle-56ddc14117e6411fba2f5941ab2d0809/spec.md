# Convert integer seconds to milliseconds and hand off the pending decision

Widen the input to u64 before multiplying by 1,000, so even u32::MAX converts without overflow. Edit only `specs/studio-lifecycle-56ddc14117e6411fba2f5941ab2d0809/normalize.rs`. Run the acceptance command in the task manifest.

## Structured handoff for Codex

Read the shared engineering notes appended to this prompt. Without a manual decision about future fractional inputs, set the separate JSON field "question" to "Which rounding convention should the next API revision use when converting fractional seconds to milliseconds?" A blank question fails the first-round handoff. Put the implementation summary in "note". If the manual decision exists, quote its exact decision reference and rounding convention in "note" and leave "question" empty. Keep the question and the decision out of the code. The integer implementation can be accepted independently of the next revision.

## Independent review by Claude

Review the integer implementation. When the manual decision exists, quote its exact decision reference and describe the convention in your structured "report", with an empty "question". Before the decision exists, report whether Codex asked the required question. If it was omitted, use the exact question from the section above in your own structured "question" so the Studio operator can resolve it.
