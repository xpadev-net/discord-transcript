# Harness Migration Candidates

Purpose:
- Stage cross-repository improvements discovered during target-repository work.
- These are not active repo rules.
- These should be picked up by a later harness-maintenance PR/issue.

## Candidates

### HMC-20261015-worker-report-self-validation

- Status: staged
- Category: validation
- Proposed home: `plugins/coding-agent-orchestration-harness/skills/subagent-report-contract/SKILL.md`
- Generalized rule:
  Workers should validate their completed YAML report against the canonical report validator before returning it.
- Trigger:
  A Worker is returning a report governed by the subagent-report-contract.
- Evidence from this repo:
  A Worker report missed required schema fields on its initial response and first correction; the Orchestrator validator caught the errors before Reviewer dispatch.
- Why this generalizes:
  This repository's Worker report incident supports the rule for reports governed by `subagent-report-contract`.
  Add evidence from the shared harness or another repository before claiming broader use.
- Suggested change:
  Add a final self-validation step to the Worker report instructions and dispatch checklist.
- Draft: none
