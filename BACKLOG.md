# Prompt — feature backlog

Status: v0.1 shipped. This is the working backlog for turning Prompt from a
strong terminal prototype into the terminal for vibe coders and agentic
development.

The backlog is ordered, not exhaustive. Anything here can move if we learn
something from users.

## Product bets

1. **The composer is the wedge.** People will switch terminals if command entry
   feels like Cursor/VS Code instead of a 1970s line editor.
2. **Agent awareness is the moat.** A terminal that understands Claude Code,
   Codex, OpenCode and friends — and helps when they stop, fail or need
   approval — is meaningfully different from a prettier terminal.
3. **Discoverability brings non-terminal natives in.** Command blocks, history,
   error explanations and a command palette should make the terminal legible
   without hiding the shell.

Non-goals for the next few months: becoming an IDE, replacing the shell,
building a general chat app, a plugin marketplace, or native Windows support.

## How to read this

- **P0 — Now:** next 2–4 weeks; the product is meaningfully worse without it.
- **P1 — Next:** following 4–8 weeks; unlocks the target audience.
- **P2 — Later:** valuable, but not before the foundation is solid.
- **P3 — Icebox:** keep on the list, revisit quarterly.
- **Effort:** S ≤ 2 days · M 3–7 days · L 1–3 weeks · XL > 3 weeks.
- **Value:** 5 = essential/differentiated, 3 = useful, 1 = marginal.
- **Confidence:** H/M/L. Prefer small, high-confidence work early.

Scores are a conversation starter, not a promise. Dependencies and sequencing
matter more than the number.

---

## P0 — Now

### CORE-01 · macOS build + runtime validation
**Value 5 · Effort L · Confidence H**

Prompt is advertised for macOS and Linux, but v0.1 has only been executed on
Linux. Make macOS a first-class, tested target.

- Add a macOS CI job that builds `libghostty-vt` with Zig and compiles Prompt.
- Verify zsh login-shell integration, Retina rendering, IME, clipboard and
  window focus on a real macOS runner or manual checklist.
- Add macOS-specific smoke instructions to the README.

**Done when:** `cargo build --release` and a scripted launch work on both Intel
and Apple Silicon macOS, and `docs/macos-smoke.md` passes.

### SHELL-05 · Integration diagnostics and recovery
**Value 5 · Effort S · Confidence H**

When the composer falls back to classic input, users currently get a vague
message. Make “why is this happening?” answerable.

- Add `prompt --diagnostics`: shell path, chosen integration, config paths,
  Ghostty version, terminal modes, log excerpt.
- Add a “Copy diagnostics” action to the classic-input card.
- Detect common causes: missing rc shim, user rc aborting, unsupported shell,
  spawn fallback, slow startup.
- Offer a one-click “use bash/zsh instead” where safe.

**Done when:** a non-expert can tell us why the composer is disabled and paste
diagnostics into an issue without reading logs.

### EDIT-01 · Inline history autosuggestion
**Value 5 · Effort S · Confidence H**

Typing should feel assisted, not blank. Show the newest matching history entry
as dim ghost text; `Right`/`Tab` accepts it, typing ignores it. Add a setting
to disable.

**Done when:** suggestions never block typing, never submit by accident, and
acceptance works with the caret in the middle of a line.

### DISC-01 · Command blocks
**Value 5 · Effort L · Confidence M**

The structural foundation for discoverability. Each submitted command becomes a
block with:

- cwd, command text, start time, duration, exit status
- output range and a “copy output” action
- rerun, edit-and-rerun, and copy-command actions
- collapse/expand for long output
- selection scoped to the block

Should degrade gracefully for non-integrated shells and full-screen programs.

**Done when:** selecting, copying and rerunning a command never depends on
guessing pixel rows, and blocks survive terminal resize.

### AGENT-01 · Detect agent CLIs and show status
**Value 5 · Effort M · Confidence M**

Detect `claude`, `codex`, `opencode`, `aider`, `gemini` and similar processes
via the PTY process tree plus window/title hints. Show a compact status chip:
running, waiting for input, needs approval, finished, failed.

- Must be disableable and must not guess when uncertain.
- Never scrape prompts or send data anywhere.
- Clicking the chip opens the agent’s cwd/session actions.

**Done when:** starting/stopping Claude Code or Codex updates the composer
status correctly, including when the agent is waiting for approval.

### EDIT-02 · Completion v2: shell-aware and described
**Value 4 · Effort L · Confidence M**

v0.1 completes PATH/history/paths. The next step is shell-aware completion:

- aliases and functions loaded from the user’s shell
- subcommands for common CLIs (git, cargo, npm, docker)
- descriptions in the popup
- fuzzy matching and ranking by history
- correct quoting for spaces/special characters

Requires a small shell→terminal metadata channel (see SHELL-04).

**Done when:** `git ch<Tab>`, alias names and quoted paths complete correctly in
zsh and bash.

### DISC-02 · History panel
**Value 4 · Effort M · Confidence H**

A browsable history surface for people who do not know what to type.

- open from the composer with `Cmd/Ctrl+Shift+H` and from a visible affordance
- search, filter by cwd/success, pin favorites
- insert without running, or run immediately
- show cwd and exit status per entry
- import existing shell history (see SHELL-03)

**Done when:** a user can find and rerun a command from last week without
knowing shell history syntax.

### EDIT-10 · Command palette
**Value 4 · Effort M · Confidence H**

`Cmd/Ctrl+Shift+P` for actions beyond typing commands: clear screen, change
font size, open history, start an agent, toggle hints, open diagnostics, switch
theme.

**Done when:** every app action has a discoverable name and keyboard path.

### CORE-02 · IME/CJK polish
**Value 4 · Effort M · Confidence M**

v0.1 has preedit plumbing; validate and finish it.

- composition preview at the caret with underline
- candidate window positioned correctly on X11/Wayland/macOS
- no duplicate commits, no lost text on focus changes
- test with Japanese, Chinese, Korean and emoji pickers

**Done when:** composing a CJK command works end-to-end on all supported
platforms.

### AGENT-03 · Natural language → command, with preview
**Value 5 · Effort L · Confidence M**

The signature vibe-coder feature. “Find the biggest files here”, “why did that
fail”, “make a branch for this ticket”.

- generate a command, explanation and risk level
- always show a preview; never auto-run
- allow edit before run
- honour cwd, shell and recent command context
- BYOK model settings; local-model option
- redact obvious secrets before sending context (see AGENT-11)

**Done when:** a non-terminal user can go from intent to a safe, reviewed
command in under 15 seconds.

### CORE-04 · Scrollback search
**Value 4 · Effort M · Confidence H**

`Cmd/Ctrl+F` search across scrollback and command output, with match count,
next/previous, highlight and “select match”. This is table stakes for agent
output that scrolls for thousands of lines.

### DIST-01 · Linux packages
**Value 4 · Effort M · Confidence H**

AUR package (Omarchy/Arch users first), `.deb`, and an AppImage or tarball.
Include the Ghostty static library, shell assets, desktop entry and icon.

### CI-01 · UI smoke tests in CI
**Value 4 · Effort M · Confidence H**

Run Prompt under Xvfb in CI, type a command with xdotool, assert on the
rendered screenshot and PTY transcript, and run the existing smoke hooks. Add
golden screenshots for first run, composer, history and a full-screen TUI.

---

## P1 — Next

### AGENT-02 · Agent session cards and resume
Show agent sessions as first-class blocks: cwd, model, elapsed time, last
activity, resume command. “Resume Claude Code in this directory” should be one
click. Detect sessions from process command lines and known local state files.

### AGENT-04 · Explain / fix the last failure
When a command exits non-zero, offer “Explain” and “Suggest a fix”. Show the
stderr + command + cwd context, a diff-style proposed command, and a copy/run
action. Never auto-run.

### AGENT-05 · Context attachments
`@` mentions for files and directories, “attach last output”, “attach git
diff”, “attach current error”. Visible context chips above the composer with
token estimates.

### AGENT-11 · Secret redaction and AI privacy controls
Redact API keys, tokens and obvious credentials from anything sent to a model.
Per-provider disclosure, local-only mode, and an audit list of what was sent.

### AGENT-06 · Approval queue for destructive commands
Detect `rm -rf`, force pushes, database drops, `sudo` etc. in AI-generated or
pasted commands. Explain the risk and require explicit confirmation for
AI-assisted runs. Keep normal typed commands frictionless.

### DISC-03 · Error cards
Turn failures into a readable card: exit code, stderr summary, likely cause,
“explain”, “fix”, “search”. This is the main bridge from terminal output to
agent assistance for less technical users.

### SHELL-01 · Fish integration
Add OSC 133 prompt markers and shim support for fish. Same composer, history
and command blocks as zsh/bash.

### SHELL-02 · Bundled zsh for releases
Ship zsh with Linux packages and the macOS app where practical. Run
`scripts/vendor-zsh.sh` in the release pipeline, verify module/function paths,
and include the licence. Keep system zsh/bash as fallbacks.

### SHELL-03 · Import shell history
On first run, import zsh/bash history into Prompt’s history (deduped, with
timestamps where available). Let users opt out.

### SHELL-04 · Shell metadata channel
A safe, versioned channel for the shell to send PATH, aliases, functions and
completion candidates to Prompt. This is the dependency for completion v2 and
PATH-aware UI.

### EDIT-03 · Soft wrapping in the composer
Long commands currently scroll horizontally. Add word-wrap with a clear
continuation marker, while keeping click/caret mapping exact.

### EDIT-04 · File mentions and drag-and-drop
Drop a file onto the composer to insert a correctly quoted path. `@` opens a
fuzzy file picker. This matters for agent prompts.

### EDIT-05 · Image paste
Paste a screenshot/image into the composer, write it to a temp file, insert the
path, and show a thumbnail chip. Designed for agent workflows.

### DISC-04 · Selection context menu
Right-click selection: copy, copy as Markdown, search, explain, open path/URL,
ask an agent. Keep the menu small and keyboard-accessible.

### DISC-05 · Clickable paths and URLs
`Cmd/Ctrl+click` a path to open it in the configured editor, and a URL to open
the browser. Highlight on hover; never trigger on plain click while selecting.

### DISC-06 · Guided onboarding
A short, skippable first-run tour: type here, Enter runs, Shift+Enter adds a
line, Up recalls, Tab completes, click output to select. Detect when a user is
stuck and surface the relevant hint once.

### CORE-03 · Forward mouse drag to TUIs
Complete mouse reporting: press, release, motion (1002/1003), wheel, SGR and
legacy encodings. Hold Shift to force text selection.

### CORE-05 · OSC 52 clipboard
Remote copy/paste support (`tmux`, SSH, `nvim`) with per-application consent and
a byte limit. Default read policy: deny; default write policy: allow with
visible feedback.

### CORE-09 · Light and auto theme
A polished light theme and “follow system” mode. The current dark theme is
opinionated; users need a daytime option.

### CORE-12 · Accessibility pass
Keyboard-only operation everywhere, high-contrast theme, screen-reader labels
for the composer/status, reduced motion, and scalable UI text independent of
terminal font size.

### DIST-02 · Signed macOS app
Universal `.app`, code signing, notarization, hardened runtime, app icon,
`Info.plist`, and a DMG. Document the Apple Developer prerequisites.

### DIST-03 · Homebrew cask
`brew install --cask prompt` with automatic updates through the cask, plus a
formula for the CLI binary.

### DIST-06 · Website and docs
Landing page with the one-sentence pitch, demo GIF, install commands, keyboard
cheatsheet, privacy policy and troubleshooting. Docs should be usable by
someone who has never opened a terminal.

### DIAG-01 · `prompt --diagnostics` and logs
Structured logs at `~/.local/share/prompt/logs`, redacted diagnostics export,
and crash-safe startup diagnostics.

---

## P2 — Later

- **EDIT-06 · Pre-run command validation** — optional shellcheck/parse warnings, never blocking.
- **EDIT-07 · Vim/Emacs keymap** — composer keymap profiles.
- **EDIT-08 · Persistent undo** — restore composer undo across sessions.
- **EDIT-09 · Snippets and prompt templates** — reusable multi-line commands and agent prompts.
- **EDIT-11 · Slash commands** — `/history`, `/agent`, `/theme`, `/clear` in the composer.
- **EDIT-12 · Copy as Markdown / share** — share command + output + exit status.
- **DISC-07 · “What happened?” summaries** — plain-language summaries of long outputs.
- **DISC-08 · Recent projects / cwd switcher** — jump between repos without `cd`.
- **AGENT-07 · Agent inbox** — notifications when an agent is blocked or finished.
- **AGENT-08 · Provider/model settings UI** — OpenAI, Anthropic, local, custom endpoints.
- **AGENT-09 · Prompt library** — common vibe-coding tasks (“add tests”, “fix types”, “explain diff”).
- **AGENT-10 · Token/cost display** — per-session cost and context usage.
- **AGENT-12 · Error → agent handoff** — send the failing command/output into a new agent session.
- **SHELL-06 · SSH/tmux guidance** — detect remote sessions, explain missing integration, optional remote install.
- **SHELL-07 · macOS bash 3.2** — test and support the system bash.
- **SHELL-08 · Session profiles** — start directory, shell, env, theme per profile.
- **SHELL-09 · Integration protocol docs** — versioned OSC/shell contract for third-party shells.
- **CORE-06 · Hyperlinks and path detection** — OSC 8 plus heuristic paths/URLs.
- **CORE-07 · Kitty graphics / inline images** — screenshots in agent output.
- **CORE-08 · Typography settings** — ligatures, font features, fallback stack.
- **CORE-10 · Keybinding customization** — conflict detection and reset.
- **CORE-11 · Notifications, bell and progress** — OSC 9/777/9;4 surfaced as toasts or desktop notifications.
- **CORE-13 · Tabs and splits** — independent sessions, drag to split, session restore.
- **CORE-14 · Session restore and crash recovery** — reopen windows, cwd, agent sessions.
- **PERF-01 · Dirty-row rendering** — only reshape rows that changed; keep the current grid cache.
- **PERF-02 · Optional GPU renderer** — for 4K/120 Hz and very large grids.
- **PERF-03 · Startup budget** — <100 ms to first frame, lazy font loading.
- **PERF-04 · Scrollback compression and caps** — expose libghostty’s compression and memory limits.
- **REL-01 · Crash reporting** — opt-in, with a local report preview.
- **REL-02 · Config migrations** — versioned config with safe upgrades.
- **REL-03 · Security review** — clipboard, OSC 52, AI redaction, paste safety, path traversal.
- **DIST-04 · Auto-update** — signed update channel, staged rollout, rollback.
- **DIST-05 · Opt-in telemetry** — activation, composer usage, crash-free sessions; no command contents, no file paths by default.
- **DIST-07 · Licence/SBOM compliance** — Ghostty, zsh and Rust dependency notices in release artifacts.

## P3 — Icebox

- Plugin/extension API and marketplace.
- Native Windows support (WSL bridge first, maybe).
- Collaborative/shared terminal sessions.
- Team command snippets and shared history.
- Voice input for prompts.
- A web-based Prompt session.
- Built-in file editor/IDE features.
- Multi-user pair programming.
- Mobile companion.
- Cloud sync of settings/history (privacy-heavy; revisit much later).

---

## Recommended sequence

### Milestone 1 — “Daily driver” (2–4 weeks)
CORE-01 macOS build, CORE-02 IME, SHELL-05 diagnostics, CI-01 UI smoke,
CORE-03 mouse drag, EDIT-01 inline suggestion, CORE-04 scrollback search.

Exit criteria: a Linux and macOS user can install Prompt, get an integrated
shell, type comfortably, use vim/ssh/tmux, and recover when integration breaks.

### Milestone 2 — “Legible terminal” (3–5 weeks)
DISC-01 command blocks, DISC-02 history panel, EDIT-10 command palette,
EDIT-02 completion v2, SHELL-03 history import, CORE-09 light theme.

Exit criteria: a less technical user can find past commands, understand what
ran, copy output and discover app actions without memorizing shortcuts.

### Milestone 3 — “Agent-aware” (4–6 weeks)
AGENT-01 agent detection, AGENT-02 session cards, AGENT-04 error explain/fix,
AGENT-03 natural language → command, AGENT-11 redaction, DISC-03 error cards.

Exit criteria: Prompt is visibly better than a normal terminal while using
Claude Code/Codex/OpenCode, and AI features are previewed, reviewed and safe.

### Milestone 4 — “Installable product” (parallel)
DIST-01 Linux packages, DIST-02 signed macOS app, DIST-03 Homebrew,
DIST-06 website/docs, DIAG-01 diagnostics export.

Exit criteria: a stranger can find Prompt, install it in one command, and get
help without joining a Discord.

---

## Prioritization table (top 20)

| Rank | ID | Feature | Value | Effort | Confidence | Why now |
|---:|---|---|---:|---|---|---|
| 1 | CORE-01 | macOS build + validation | 5 | L | H | Product promise; blocks launch |
| 2 | SHELL-05 | Integration diagnostics | 5 | S | H | Zero-config trust |
| 3 | EDIT-01 | Inline history suggestion | 5 | S | H | Daily typing quality |
| 4 | DISC-01 | Command blocks | 5 | L | M | Foundation for discovery/AI |
| 5 | AGENT-01 | Agent detection/status | 5 | M | M | Core differentiation |
| 6 | EDIT-02 | Completion v2 | 4 | L | M | Reduces typing and errors |
| 7 | DISC-02 | History panel | 4 | M | H | Vibe-coder discoverability |
| 8 | EDIT-10 | Command palette | 4 | M | H | Discoverability of the app |
| 9 | CORE-02 | IME/CJK polish | 4 | M | M | Text entry must be global |
| 10 | AGENT-03 | NL → command preview | 5 | L | M | Signature feature |
| 11 | CORE-04 | Scrollback search | 4 | M | H | Agent output is long |
| 12 | CI-01 | UI smoke tests | 4 | M | H | Protects every release |
| 13 | DIST-01 | Linux packages | 4 | M | H | Getting users |
| 14 | SHELL-01 | Fish integration | 3 | M | M | Broader shell coverage |
| 15 | AGENT-04 | Error explain/fix | 4 | M | M | Clear user value |
| 16 | DISC-03 | Error cards | 4 | M | M | Plain-language failures |
| 17 | SHELL-02 | Bundled zsh | 3 | M | H | True zero-config Linux |
| 18 | CORE-03 | Mouse drag forwarding | 3 | S | H | Terminal fidelity |
| 19 | DIST-02 | Signed macOS app | 4 | L | M | Trust and installation |
| 20 | CORE-05 | OSC 52 clipboard | 3 | M | M | SSH/tmux workflows |

---

## Metrics and signals

Instrument only with opt-in consent, and never collect command contents or file
paths by default.

- **Activation:** first successful command within 30 seconds of first launch.
- **Composer share:** % of commands submitted through the composer vs classic
  input; target >70% for integrated shells.
- **Assist quality:** completion acceptance rate; inline suggestion acceptance;
  history-panel reruns.
- **Agent value:** agent sessions detected, resumed and unblocked; time from
  agent failure to next action.
- **Retention:** D7 and D30 weekly active users.
- **Reliability:** crash-free sessions, shell-integration failure rate,
  startup-to-first-frame p50/p95.
- **Trust:** AI preview acceptance vs rejection; redaction events; zero
  auto-run AI commands.

## Open decisions

1. **AI provider model:** BYOK only at first, or a bundled model with a
   subscription? Recommendation: BYOK + local, no bundled billing in v1.
2. **Command blocks rendering:** draw inside the terminal grid (hard, exact)
   vs a structured layer above the grid (easier, more control). Recommendation:
   structured layer using semantic prompt ranges, with a terminal fallback.
3. **Bundled zsh:** commit a binary vs build in the release pipeline. Current
   decision: pipeline, not repository.
4. **Telemetry:** none by default; opt-in aggregated events only. Revisit before
   public launch.
5. **Scope of “agent awareness”:** process detection + status first; no prompt
   interception or token streaming until the block model is stable.
6. **Minimum macOS:** decide deployment target (e.g. macOS 12+) before signing.
