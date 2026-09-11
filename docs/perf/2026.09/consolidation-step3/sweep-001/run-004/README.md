The [retained workspace command](command.json) returned 101 after [139.969 seconds](run.json) at `48ebff5bed99f45c0b3685323ced4e5b837c412a`. The [final classification](classification-002/report.md) names all 101 failures and 84 explicit skips.

All failures retain their run 003 identity and cause. The 2,338 reported passes include those skips, while six doctests pass and two cases remain filtered. All 34 moved app cases report passes, and 17 earlier moved failures still refuse missing inputs.

The [source record](source-stability.json) preserves unchanged HEAD and 29,046 tracked files outside Beads. [Source comparison](classification-002/source-comparison.json) verifies the reused mapping inputs against captured Git objects. The [copy record](publication.json) preserves original bytes and POSIX modes, although Git stores only the executable permission bit.

The [first review attempt](review-attempt-001/result.json) used a moved historical path at the wrong commit and failed before writing a final classification. The corrected review checks historical paths at their recorded commits. It leaves no [unresolved review or parser entries](classification-002/pending-review.json).

This failed workspace run does not establish completion of stage 3 or unexecuted application work. The earlier failed runs remain unchanged.
