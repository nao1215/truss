---
name: impl
description: >
  Rust implementation workflow for the truss image toolkit project. Use this skill whenever
  the user asks to implement a new feature, add functionality, fix a bug, or make code changes
  in the truss project. This includes requests like "implement X", "add support for Y",
  "fix the Z bug", "create a new endpoint", "add a CLI command", or any task that involves
  writing or modifying Rust code in this project. Trigger even for small changes — the
  structured workflow ensures quality regardless of scope.
---

# Truss Implementation Skill

This skill guides you through a structured implementation workflow for the truss image toolkit.
Every phase includes a critical review by a separate agent to catch issues early and maintain
high code quality.

The truss project follows a **library-first architecture**: core logic lives in `src/core.rs`
and `src/codecs/`, while I/O concerns live in `src/adapters/`. Understanding this boundary
is essential for placing code correctly.

## Workflow Overview

```
1. Research  →  2. Design (PLAN)  →  3. Implement  →  4. Test  →  5. Final Review
     ↓               ↓                    ↓              ↓             ↓
  Read docs     Agent reviews        Agent reviews   Agent reviews  Agent reviews
```

Every phase produces artifacts. Every phase gets reviewed. Move to the next phase only
after addressing review feedback.

---

## Phase 1: Research

Before writing any code, build a thorough understanding of the current state.

### What to read

1. **AGENTS.md** — Project-wide LLM instructions and constraints
2. **README.md** — Project overview and scope
3. **doc/** directory — Design specs that govern implementation decisions:
   - `doc/cli.md` — CLI command design and option naming
   - `doc/api.md` — HTTP API specification
   - `doc/runtime-architecture.md` — Library-first architecture and module boundaries
   - `doc/desgin.md` — Server runtime design (note: filename has a typo, that's intentional)
   - `doc/implementation-log.md` — What's been done, what's pending, known gaps
4. **Relevant source files** — Read the modules you'll be modifying or extending

### What to extract

- Where does the new code belong? (core vs adapter vs codec)
- What existing types, traits, and functions can be reused?
- Are there design constraints in the docs that affect this change?
- What's the current test coverage and testing patterns?

---

## Phase 2: Design (PLAN Mode)

Enter Plan mode and create a written design before touching code.

### Design document structure

Write a concise design covering:

1. **Goal** — One sentence describing what this change accomplishes
2. **Placement** — Which files/modules will be created or modified, and why
3. **Public API** — New types, functions, or endpoints with their signatures
4. **Internal design** — Key algorithms, data flow, error handling approach
5. **Testing strategy** — What unit tests, doc tests, and integration tests are needed
6. **Impact** — What existing code is affected; any breaking changes

### Design review

After creating the design, spawn a separate agent to critically review it:

```
Spawn an Agent (subagent_type: "general-purpose") with this prompt:

"You are a critical code reviewer for a Rust project called truss (an image toolkit).
Review the following design and identify problems. Be thorough and critical — your job
is to find flaws, not to be encouraging.

Check for:
- Violations of the library-first architecture (core should not depend on adapters)
- Naming inconsistencies with existing conventions (lowerCamelCase for JSON, kebab-case for CLI)
- Missing error cases or unsafe assumptions
- Scope creep beyond what was requested
- Gaps in the testing strategy
- Conflicts with existing design docs

Design to review:
<paste the design here>

Read these files for context:
- /home/nao/ghq/github.com/nao1215/truss/AGENTS.md
- /home/nao/ghq/github.com/nao1215/truss/doc/runtime-architecture.md
- /home/nao/ghq/github.com/nao1215/truss/src/lib.rs

Respond with:
1. CRITICAL issues (must fix before implementing)
2. WARNINGS (should fix but not blockers)
3. SUGGESTIONS (nice to have improvements)
"
```

Address all CRITICAL issues before proceeding. Incorporate WARNINGS where reasonable.

---

## Phase 3: Implementation

Write the code following truss conventions and Rust best practices.

### Code conventions

These patterns are established in the existing codebase — follow them for consistency:

**Documentation comments** — Every public item gets a `///` doc comment:
```rust
/// Converts a raster image to the specified output format.
///
/// This function applies the transform options (resize, rotation, format conversion)
/// to the input artifact and returns the transformed bytes.
///
/// # Arguments
///
/// * `input` - The source image artifact containing raw bytes and metadata
/// * `options` - Transform parameters specifying the desired output
///
/// # Errors
///
/// Returns [`TransformError::DecodeFailed`] if the input bytes cannot be decoded.
/// Returns [`TransformError::EncodeFailed`] if encoding to the target format fails.
///
/// # Examples
///
/// ```
/// use truss::{Artifact, TransformOptions, MediaType, transform_raster};
///
/// let artifact = Artifact { /* ... */ };
/// let options = TransformOptions { format: Some(MediaType::Png), ..Default::default() };
/// let result = transform_raster(&artifact, &options);
/// ```
pub fn transform_raster(input: &Artifact, options: &TransformOptions) -> Result<Vec<u8>, TransformError> {
    // ...
}
```

**Error handling:**
- Use `TransformError` enum for core errors
- Add new variants when existing ones don't fit
- Error messages should be descriptive and actionable

**Naming:**
- Structs/enums: `PascalCase`
- Functions/methods: `snake_case`
- Constants: `SCREAMING_SNAKE_CASE`
- JSON fields: `lowerCamelCase` (via serde rename)
- CLI flags: `kebab-case`

**Derive traits:**
- Data structs: `#[derive(Debug, Clone, PartialEq)]`
- Add `Eq` when all fields are `Eq`
- Add `Default` when there's a natural default
- Add `serde::Serialize, serde::Deserialize` for types that cross I/O boundaries

**Module placement:**
- Pure transformation logic → `src/core.rs` or `src/codecs/`
- CLI parsing and output → `src/adapters/cli.rs`
- HTTP routing and request handling → `src/adapters/server.rs`
- New codec backends → `src/codecs/` (new file + register in `mod.rs`)

### Implementation review

After writing the implementation (before tests), spawn a reviewer agent:

```
Spawn an Agent (subagent_type: "general-purpose") with this prompt:

"You are a senior Rust developer reviewing code for the truss image toolkit.
Review the following changes critically. Focus on correctness and maintainability.

Check for:
- Rust best practices (ownership, borrowing, lifetimes, error handling)
- Potential panics (unwrap, expect, indexing) in non-test code
- Missing or incorrect documentation comments
- Security issues (path traversal, injection, unchecked input)
- Performance concerns (unnecessary allocations, copies)
- Whether the implementation matches the design
- AGENTS.md compliance (English comments, no GIF support, doc comments on public items)

Files to review:
<list the modified/created files with their full paths>

Read each file and provide:
1. CRITICAL issues (bugs, security problems, incorrect behavior)
2. WARNINGS (code smells, missing docs, suboptimal patterns)
3. SUGGESTIONS (style improvements, idiomatic Rust alternatives)
"
```

Fix all CRITICAL issues. Address WARNINGS.

---

## Phase 4: Testing

Write comprehensive tests at three levels. The existing test suite has ~96 unit tests,
23 integration tests, and 12 doc tests — maintain or exceed this coverage standard.

### Unit tests

Place in `#[cfg(test)]` module at the bottom of the source file:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_descriptive_name_for_happy_path() {
        // Arrange
        let input = /* ... */;
        // Act
        let result = function_under_test(input);
        // Assert
        assert_eq!(result, expected);
    }

    #[test]
    fn test_descriptive_name_for_error_case() {
        let result = function_under_test(invalid_input);
        assert!(result.is_err());
    }
}
```

Cover: happy path, edge cases, error paths, boundary values.

### Documentation tests

Every `# Examples` section in doc comments should be a runnable test:

```rust
/// # Examples
///
/// ```
/// use truss::MediaType;
///
/// let media = MediaType::from_str("image/png").unwrap();
/// assert_eq!(media, MediaType::Png);
/// ```
```

Make sure doc tests actually compile and pass. Use `# ` prefix to hide setup lines
that aren't relevant to the reader.

### Integration tests

Place in `tests/` directory. Follow existing naming pattern:
- CLI tests: `tests/cli_<feature>.rs`
- Server tests: `tests/server_<feature>.rs`

Integration tests exercise the public API end-to-end:

```rust
// tests/cli_new_feature.rs
use std::process::Command;

#[test]
fn test_new_feature_end_to_end() {
    let output = Command::new(env!("CARGO_BIN_EXE_truss"))
        .args(&["convert", "input.jpg", "-o", "output.png"])
        .output()
        .expect("failed to execute");

    assert!(output.status.success());
}
```

### Test review

Spawn a reviewer agent for the tests:

```
Spawn an Agent (subagent_type: "general-purpose") with this prompt:

"You are a QA engineer reviewing test code for a Rust image toolkit called truss.
Review the tests critically for completeness and correctness.

Check for:
- Missing edge cases (empty input, maximum values, invalid formats)
- Missing error path tests
- Tests that would pass even if the code were broken (tautological tests)
- Proper use of assertions (assert_eq over assert where possible)
- Integration test coverage of the feature's user-facing behavior
- Doc tests that actually demonstrate useful usage patterns
- Test isolation (no shared mutable state between tests)

Test files to review:
<list test files>

Also read the implementation files to identify untested code paths:
<list implementation files>

Respond with:
1. MISSING tests (specific scenarios that need coverage)
2. WEAK tests (tests that exist but don't adequately verify behavior)
3. SUGGESTIONS (better assertion strategies, test organization)
"
```

Add any MISSING tests identified by the reviewer.

---

## Phase 5: Final Verification

1. Run `cargo test` — all tests must pass
2. Run `cargo clippy -- -D warnings` — no warnings
3. Run `cargo doc --no-deps` — documentation builds without warnings
4. Update `doc/implementation-log.md` with what was done (per AGENTS.md)

### Final review

Spawn one last reviewer agent:

```
Spawn an Agent (subagent_type: "general-purpose") with this prompt:

"You are doing a final review of a completed implementation for the truss project.
This is the last check before the work is considered done.

Verify:
- All AGENTS.md rules are followed
- Documentation comments are present on all new public items
- The implementation matches the original design
- Tests pass (read the test output if available)
- implementation-log.md has been updated
- No TODO or FIXME comments were left without explanation
- No unnecessary files were created

Files changed in this implementation:
<list all files>

Read each file and give a final GO / NO-GO verdict with specific reasons if NO-GO.
"
```

---

## Key Reminders

- **GIF is out of scope** — reject any GIF-related implementation requests
- **Comments in English** — all code comments and documentation in English
- **Library-first** — core must not import from adapters
- **Update the log** — always update `doc/implementation-log.md`
- **Don't merge gaps silently** — record any known limitations in the log
