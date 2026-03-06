# AGENTS.md

## Repository Instructions for LLMs

- Create or update a written plan before substantial implementation work.
- Keep a shared implementation log in `doc/implementation-log.md` so other LLMs can continue the work without re-discovery.
- When making progress, append concise work log entries that describe decisions, changes, and remaining work.
- Write clear and detailed Rust documentation comments for public items.
- Public functions must document behavior, important inputs, return values, and failure cases at a practical level of detail.
- Write comments in English.
- Prefer documentation comments (`///`) over casual inline comments unless the code is private and the explanation is genuinely necessary.
- Write thorough tests for new behavior and keep coverage high.
- Add unit tests for local logic, integration tests for externally visible behavior, and doc tests for public APIs when adding or changing functionality.
- Do not merge incomplete work silently; record known gaps and follow-up tasks in `doc/implementation-log.md`.
