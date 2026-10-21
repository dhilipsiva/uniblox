# Research reports

The raw answers that five LLM research runs gave to the same brief: the open engineering questions for a browser-first Rust/WASM UGC game platform. All five were added on 2026-07-09, before any code. They are source material, not decisions: they are stale in places (for example Bevy 0.15 and lightyear 0.17), and they disagree with each other.

The decisions taken from them live elsewhere:
- [../final-buildspec.md](../final-buildspec.md): the resolved verdicts, the stack and the risk register; its "Corrections" section lists what these reports got wrong.
- [../final-todo.md](../final-todo.md): the build plan.
- [../CONTEXT.md](../CONTEXT.md): the rationale and the rejected alternatives.
- [../../DECISIONS.md](../../DECISIONS.md): the ADR log, which cites a report where one shaped a decision.

| File | Source | Size | Opens with |
|---|---|---|---|
| [chatgpt.txt](chatgpt.txt) | ChatGPT | 22 KB | An executive summary recommending Bevy 0.19 on wgpu. |
| [claude.txt](claude.txt) | Claude | 51 KB | "Engineering Resolution of the Open Questions". |
| [deepseek.txt](deepseek.txt) | DeepSeek | 47 KB | "Research Response", question by question. |
| [gemini.md](gemini.md) | Gemini | 46 KB | "Platform Architecture and Subsystem Resolution for Tri-Mode Browser-First UGC Game Network". |
| [grok.txt](grok.txt) | Grok | 21 KB | A recommended stack per workstream (A to L). |

The source names come from the file names. The reports are kept verbatim; do not edit them, and do not cite them as current fact without checking `final-buildspec.md` first.
