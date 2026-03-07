---
description: Quick overview of all TODOs, FIXMEs, and unimplemented work
---

# List All Tasks

Call the `list_tasks` MCP tool from the `todo-comments` server with `path` set to the project root directory.

Then format the results as an organized overview:

1. **Summary** — total count, breakdown by marker type, breakdown by crate/directory
2. **By Priority** — FIXMEs first (likely bugs), then TODOs grouped by crate
3. **Each task** — show as `file:line` with the extracted comment text

Keep it scannable. Use markdown formatting with file:line references.
