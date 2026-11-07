#!/bin/sh
# Fake `claude` for the end-to-end tests. Like Claude Code it reports hooks through the
# hook command (SessionStart first), shows an input prompt with the "manual mode on"
# footer, takes bracketed-paste input, and starts and ends a turn per line. A line starting
# with "ask" raises a permission dialog (PermissionRequest hook + the dialog on screen) and
# waits for one key, appended to $FAKE_CLAUDE_LOG.keys; a line containing "hold" starts a
# turn that never ends. Its arguments are appended to $FAKE_CLAUDE_LOG.
hook() { printf '%s' "$1" | "$MIRANTE_HOOK_BIN"; }
SID=none
SRC=startup
prev=
for a in "$@"; do
  case "$prev" in
    --session-id) SID=$a ;;
    --resume) SID=$a; SRC=resume ;;
  esac
  prev=$a
done
echo "$*" >> "$FAKE_CLAUDE_LOG"
# Screen: a note, the input prompt and the footer (U+23F8 and U+276F as octal UTF-8).
prompt() { printf '\033[2J\033[H%s\r\n\342\235\257 \r\n  \342\217\270 manual mode on\r\n' "$1"; }
hook "{\"hook_event_name\":\"SessionStart\",\"session_id\":\"$SID\",\"source\":\"$SRC\"}"
prompt "fake claude $SID"
while IFS= read -r line; do
  line=$(printf '%s' "$line" | tr -d '\033' | sed 's/\[20[01]~//g')
  hook "{\"hook_event_name\":\"UserPromptSubmit\",\"session_id\":\"$SID\",\"prompt\":\"typed\"}"
  case "$line" in
    ask*)
      hook "{\"hook_event_name\":\"PermissionRequest\",\"session_id\":\"$SID\",\"tool_name\":\"Bash\",\"tool_use_id\":\"tu1\",\"tool_input\":{\"command\":\"touch x\"}}"
      printf '\033[2J\033[H Bash command\r\n   touch x\r\n Do you want to proceed?\r\n \342\235\257 1. Yes\r\n   2. No\r\n Esc to cancel\r\n'
      stty raw -echo
      k=$(dd bs=1 count=1 2>/dev/null)
      stty sane
      echo "$k" >> "$FAKE_CLAUDE_LOG.keys"
      if [ "$k" = 1 ]; then
        hook "{\"hook_event_name\":\"PostToolUse\",\"session_id\":\"$SID\",\"tool_name\":\"Bash\",\"tool_use_id\":\"tu1\"}"
      fi
      prompt "answered $k"
      hook "{\"hook_event_name\":\"Stop\",\"session_id\":\"$SID\"}" ;;
    *hold*)
      prompt "holding" ;;
    *)
      prompt "echo: $line"
      hook "{\"hook_event_name\":\"Stop\",\"session_id\":\"$SID\"}" ;;
  esac
done
