## Exit codes (stable)

| Code | Meaning |
|---|---|
| 0 | success |
| 1 | user error (bad args, not found) |
| 2 | auth error (not logged in / token expired) |
| 3 | network / API error |
| 4 | conflict (publish needs `--force`) |
| 5 | plan limit (free tier hit cap) |
| 6 | partial failure (bulk op — some succeeded, some failed) |
| 64 | invalid input (POSIX EX_USAGE) |
| 70 | internal CLI bug |

Treat any other non-zero exit as code 70.

### About exit 6 (partial failure)

Bulk operations (`knack mark a,b,c succeeded`, future bulk-export, etc.)
return exit 6 when SOME items succeeded and others didn't. The
succeeded items already landed — they're durable. The structured
`--json` envelope carries the per-item breakdown:

```json
{
  "ok": false,
  "data": {
    "marked": ["run-a", "run-b"],
    "failed": [{"run_id": "run-c", "error": "..."}],
    "total": 3
  }
}
```

For a CI script: `exit 6` means "I did SOME of what you asked." Don't
re-run the whole batch; parse the JSON and retry only the `failed[]`
entries (or accept the partial state and move on). The distinction
from exit 70 (the CLI crashed) is what lets you make that call.
