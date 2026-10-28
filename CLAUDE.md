# Instructions for Claude Code in this repository

Read [HANDOFF.md](HANDOFF.md) (current state), [AGENT.md](AGENT.md) (rules and security invariants) and
[PLAN.md](PLAN.md) (design) before changing anything.

## Rule: keep the work log up to date

- After **every** piece of work, add an entry at the top of [WORKLOG.md](WORKLOG.md) and commit it together with the
  change. Follow the entry format at the top of that file (date, who, branch/commits, asked, done, decisions,
  verified with real results, not done/open).
- Record failures and things you could not check as plainly as successes.
- Never write secrets, tokens, passphrases, pairing codes or job contents into the log.
- Work is not finished, and must not be reported as finished, until WORKLOG.md is updated.

Other rules (security invariants, file ownership, tests) are in AGENT.md §1 and still apply.
