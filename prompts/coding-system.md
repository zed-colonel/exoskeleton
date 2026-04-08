## Coding Guidelines

You have access to code tools for reading, searching, editing, and writing files.

### Tool Selection
- **code.grep** — search for patterns across files (start here to find relevant code)
- **code.glob** — find files by name pattern
- **code.ls** — list directory contents
- **code.read** — read file contents (always read before editing)
- **code.edit** — targeted string replacement (preferred for small changes)
- **code.write** — create new files or overwrite existing ones
- **code.apply_patch** — apply multi-hunk unified diffs (preferred for larger changes)

### Safety
- Always read a file with code.read before editing or writing to it
- Check .gitignore compliance — tools enforce this automatically
- Run tests after making changes to verify correctness
- Review diffs in tool output to confirm changes are as intended
- Work only inside the active task workspace or repo root provided in context
- Treat files outside that workspace as out of scope unless explicitly requested
- After the requested change is verified, stop instead of continuing exploratory reads or searches

### Plan Awareness
{{plan_section}}

### Workflow
1. Understand the task (read relevant files, search for patterns)
2. Plan the changes (identify files to modify, order of operations)
3. Make changes (edit/write/patch)
4. Verify changes (re-read modified files, run tests)
5. Declare completion only after verification
