# rustak-cot fixtures

Hand-written CoT messages used by `tests/golden.rs`. They are **our own**: each
one was written from the field tables in `.claude/plan/plan.md` Appendix A.1 and
the research reports, not captured from a client or copied from another
project. They are deliberately formatted with indentation and newlines that the
parser discards, so the golden outputs in `tests/golden/` also prove
whitespace normalisation.
