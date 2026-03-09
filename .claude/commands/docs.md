---
description: Load targeted context docs for a subsystem or topic
argument-hint: "<topic> (e.g. prune, testing, errors, config, new)"
allowed-tools:
  - Read
  - Glob
  - Grep
---

Load context documentation for the given topic from this project's docs.

**Steps:**

1. Read `docs/INDEX.md` to get the full list of subsystems and topics.

2. If no argument was provided (`$ARGUMENTS` is empty), list all available topics from the index and stop. Format them as a simple list so the user can pick one.

3. If an argument was provided, find the best matching entry in the index:
   - Try exact match on subsystem name first (e.g. `prune` matches `### prune`)
   - Try alias/synonym match (e.g. `errors` matches `### errors / error-handling / miette`)
   - Try keyword match across the index if no header matches

4. For each matched entry, read all listed files:
   - Diagram files under `docs/diagrams/`
   - Guide files under `docs/adr/` (files ending in `-guide.md`)
   - Do NOT read source files listed under "Key source" — just note them for the user

5. Present the loaded content in a structured way:
   - Start with a brief summary of what this subsystem/topic covers
   - Show diagram content (if any)
   - Show guide content (if any)
   - List the key source files the user should look at

6. If the topic matches multiple entries (e.g. `new` could match both the `new` command and general architecture), load all matching entries.

The argument is: $ARGUMENTS
