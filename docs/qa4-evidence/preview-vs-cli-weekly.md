# Preview vs CLI — qa-weekly (same session)

## GUI (`p4b-dialog-preview-weekly.png`)
Next 5 fires (local 08:00 Europe/Helsinki, offset +03:00):
1. 2026-07-29 08:00 → UTC 05:00:00Z
2. 2026-07-31 08:00 → UTC 05:00:00Z
3. 2026-08-03 08:00 → UTC 05:00:00Z
4. 2026-08-05 08:00 → UTC 05:00:00Z
5. 2026-08-07 08:00 → UTC 05:00:00Z

## CLI
```
next fires for "qa-weekly" (cdd99edf-1a9b-469f-95fe-d7a8acdcf9dd):
  1: 2026-07-29T05:00:00+00:00
  2: 2026-07-31T05:00:00+00:00
  3: 2026-08-03T05:00:00+00:00
  4: 2026-08-05T05:00:00+00:00
  5: 2026-08-07T05:00:00+00:00
```
Match: local+offset+UTC agree for all five.
