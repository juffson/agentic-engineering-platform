# Preserve deliberate empty arguments in AEP acceptance commands

An argument vector can contain an empty string. For example, a shell acceptance check may deliberately pass an empty positional argument to verify default behavior. TaskManifest validation currently rejects every empty argument, so valid commands cannot be dispatched. Update src/task.rs only.

- AC-1: Primary and additional acceptance commands accept a deliberate empty argument after the executable. An executable name must still be present and nonempty.
- AC-2: Both primary and additional commands still reject NUL bytes in any argument, and their existing criterion and check ID rules stay in place.
