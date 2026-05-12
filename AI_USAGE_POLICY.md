# AI Usage Policy

This document describes how AI coding assistants may be used when contributing
to **Cano**. It is loosely inspired by the Linux kernel's
[Coding Assistants policy](https://docs.kernel.org/process/coding-assistants.html),
but adapted to the realities of this project:

- Cano is a small, Apache-2.0 **OR** MIT dual-licensed Rust crate.
- There is no Developer Certificate of Origin (DCO) and no `Signed-off-by:`
  requirement.
- All contributions arrive through normal GitHub pull requests.

The policy applies to **every contributor**, including the maintainer.

---

## 1. The Human Author is Always Responsible

> **A contributor who opens a pull request is fully responsible for every line
> of code in that pull request — regardless of whether they typed it, an AI
> generated it, they copied it from a chat window, or an agent committed it on
> their behalf.**

This is the single most important rule in this document.

Concretely, by opening a PR you confirm that you:

1. **Understand the change.** You can explain *why* the code is written the
   way it is, what it does, and what its failure modes are. "The AI wrote it"
   is never an acceptable answer to a review comment.
2. **Reviewed the diff in full.** You read the entire diff, not just the
   summary the assistant produced.
3. **Ran the project checks locally** (see `CLAUDE.md` and the CI workflow):
   - `cargo fmt --all -- --check`
   - `cargo clippy --all-targets --all-features -- -D warnings`
   - `cargo test --lib` and `cargo test --doc`
   - `cargo check --examples`
4. **Have the right to submit the code** under the project's dual Apache-2.0
   / MIT license (see [the Contribution section in
   `README.md`](README.md#contribution)). You must not paste in code whose
   provenance or license you cannot vouch for — this includes verbatim
   snippets reproduced by a model from training data that you have not
   independently verified are compatible.

If a bug, regression, or license issue is later traced to AI-generated code
in your PR, the responsibility is yours, not the tool's.

---

## 2. Allowed Uses of AI Assistants

AI assistants are explicitly **allowed** and even encouraged for, among
others:

- Drafting code, tests, examples, documentation, and commit messages.
- Refactoring, renaming, and other mechanical edits.
- Exploring a crate, an API, or a part of the codebase.
- Generating boilerplate (e.g. trait impls, derive expansions, doc
  comments).
- Reviewing diffs, suggesting alternative designs, and explaining errors.
- Driving multi-step changes through agentic workflows (e.g. Claude Code,
  Codex, local agents).

There is no restriction on whether the model runs in the cloud or locally,
nor on which vendor provides it. Use whichever tool makes you productive.

---

## 3. Rules Every AI-Assisted Contribution Must Follow

Independent of which assistant produced the code, the following must hold
before the PR is opened:

1. **Style & lints.** The code passes `cargo fmt` and
   `cargo clippy --all-targets --all-features -- -D warnings`.
2. **Tests.** New behaviour is covered by tests; existing tests still pass
   (`cargo test --lib` and `cargo test --doc`). Doc examples in changed
   public items must compile.
3. **No invented APIs.** If the assistant references a function, type, crate
   feature, or CLI flag, you must verify it actually exists at the version
   pinned in `Cargo.toml`. Hallucinated APIs are a common AI failure mode —
   reject them.
4. **No fabricated benchmarks, citations, or security claims.** If a number,
   quote, RFC, or CVE appears in the PR description or in code comments, it
   must be one you have personally verified.
5. **Project conventions.** Follow the architectural conventions described
   in [`CLAUDE.md`](CLAUDE.md) — feature flags, module layout, trait shape,
   error types, and the existing public-API patterns are part of the
   contract.
6. **Licensing.** Do not submit code the assistant reproduced verbatim from
   an incompatible source. If in doubt, rewrite it yourself.
7. **Secrets.** Never paste API keys, customer data, or other secrets into a
   third-party AI service while preparing a contribution.

A PR that fails any of these is rejected on the same grounds as a
hand-written PR that fails them — the assistant being involved is neither
an excuse nor an aggravating factor.

---

## 4. Optional Attribution

If you wish to disclose that an assistant was involved, you may add an
`Assisted-by:` trailer to your commit message, modelled on the kernel's
convention:

```
Assisted-by: <agent name>:<model id> [optional tools]
```

Examples:

```
Assisted-by: Claude Code:claude-opus-4-7
Assisted-by: Qwen3-Coder-30B (local)
Assisted-by: DeepSeek-V3 (local) rust-analyzer
```

This is **optional**. Cano does not require attribution trailers, and their
absence does not change the responsibility model described in §1. They exist
purely for transparency and reproducibility, the same way one might mention
which IDE plugin or refactoring tool produced a change.

Do **not** add a `Signed-off-by:` line on behalf of an AI agent — Cano has
no DCO, so the trailer has no meaning here, and an AI cannot legally certify
authorship anyway.

---

## 5. Maintainer Disclosure

For transparency, the maintainer of this repository routinely uses:

- **Claude Code** via the Anthropic API, and
- **Qwen** and **DeepSeek** models running locally.

This is noted in the [AI Disclosure section of `README.md`](README.md#ai-disclosure).
Contributors are not required to match this stack — use whichever assistants
you prefer, as long as the rules above are met.

---

## 6. Changes to This Policy

This policy is intentionally short and is expected to evolve as tools and
norms change. If you think a rule is missing, wrong, or out of date, open
an issue or a PR — the same way you would for any other change to the
project.
