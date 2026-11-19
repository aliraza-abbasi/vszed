# ACP display terminal memory: upstream candidate

Branch `fix/acp-display-terminal-memory`. A fix to upstream code, not a fork feature: every commit
is meant to go upstream as is. ACP agents (claude-agent-acp over v2, Codex, Cursor, ...) get a
display-only `terminal::Terminal` for every command, and the thread keeps it for its lifetime.
Resuming a long session replays every command, so a session with 11,720 Bash calls held about
4.4 GB of terminals in Zed.

Memory per finished display terminal with 12 lines of output (2,000 terminals in a debug test
build, resident memory measured with `ps`; probe kept outside the repo):

| State | Per terminal |
|---|---|
| upstream before #60019 (no shrink on exit) | ~1,350 KB |
| upstream now (shrink on exit, #60019) | ~380 KB |
| + drop the parse buffer on exit | ~130 KB |
| + compile the URL regex on first use | ~96 KB |
| + compact when nothing has drawn it | ~13 KB |

## Commits

1. `fix(terminal): drop the parse buffer when a display terminal finishes`. #64405 drops the
   parser (and vte's 2 MiB synchronized-update buffer) in `release_pty_resources`, which display
   terminals never call. `shrink_to_used`, which runs when their output completes, now drops it.
2. `perf(terminal): compile the URL hyperlink regex on first use`. `RegexSearches::url_regex` is
   built by the first hyperlink search instead of at creation.
3. `perf(terminal): compact finished display terminals that nothing has drawn`.
   `Terminal::compact_finished_output` (called by `acp_thread::Terminal` at exit instead of
   `shrink_to_used`) keeps the grid as text plus SGR/OSC 8 codes (`alacritty::grid_escape_codes`)
   and swaps the emulator for a one-cell one. `sync` and `write_raw_output` rebuild it;
   `total_lines`/`used_lines`/`viewport_lines` come from the compacted copy; `get_content` and
   `last_n_non_empty_lines` read a throwaway rebuild. Drawn terminals only shrink. Terminals that
   get output after exiting stay live, so a split escape sequence keeps its parser.

Files: `crates/terminal/src/terminal.rs`, `crates/terminal/src/alacritty.rs`,
`crates/terminal/src/alacritty/hyperlinks.rs`, `crates/acp_thread/src/terminal.rs`, a test in
`crates/acp_thread/src/acp_thread.rs`, and `terminal` with `test-support` in
`crates/acp_thread/Cargo.toml` dev-dependencies.

## Upstream context

- #57099 "ACP terminal scrollback buffers never freed after tool call exits" (beardfaceguy),
  closed by #60019 "Shrink ACP terminal scrollback to used on exit" (maja-sh, merged by
  bennetbo). Earlier attempts: #59725 (maja-sh), #57104 (miguelraz).
- #64405 "terminal: Release the parse buffer once a command finishes" (zhujiatao) covers
  process terminals only.
- Related reports: #43772 (large agent threads), #64883 (restoring one large thread),
  #61702 (restoring remote ACP threads).
- Nothing has been posted upstream. Ali decides when.
