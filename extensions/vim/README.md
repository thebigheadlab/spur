# Vim preset

Optional Vim keys for Spur. Nothing here is part of Spur's core or default keys; load it with:

```sh
spur --preset vim FILE
```

It adds its own modes (`normal`, `insert`, `operator`, `visual`, `visual_line`, `cmdline`), a keymap layer (`keymap.toml`, bracket key notation), and commands in the `vim.` namespace: the operator grammar, text objects, find-char, the `:` line and `/` search, `.` repeat and macros. It plugs into the core through two generic hooks: a motion hook (an operator takes over a motion's range) and a dispatch observer (recording changes and macros).

Scenario tests in `tests/vim.rs` check behaviour against Vim.
