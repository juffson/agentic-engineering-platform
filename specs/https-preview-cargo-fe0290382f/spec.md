# Keep AEP Studio's log preview on a UTF-8 boundary

AEP Studio shows at most the last 48 KiB of an artifact. When the byte limit starts partway through a valid multibyte character, the preview currently inserts a replacement character that was never in the log. Fix this in src/studio.rs and keep the artifact path checks, the preview size limit and the truncation flag.

- AC-1: A truncated preview whose first retained byte belongs to the middle of a valid UTF-8 character omits that partial character. A character fully inside the byte budget is preserved.
- AC-2: Malformed bytes genuinely present in the retained text still decode lossily. Short logs retain their current behavior.
- Edit only src/studio.rs. The acceptance program is pinned and checks the public preview API.
