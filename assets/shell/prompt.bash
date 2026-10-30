# Prompt shell integration for bash.
#
# Loaded from Prompt's shim rcfile. PROMPT_COMMAND runs before every prompt,
# which gives us both the exit status of the command that just finished and a
# reliable "the shell is ready for input" marker.

if [[ "${PROMPT_SHELL_INTEGRATION:-0}" == "1" ]]; then
  export PROMPT_SHELL_INTEGRATION=0

  _prompt_precmd() {
    local _prompt_status=$?
    printf '\033]133;D;%s\033\\\033]133;A\033\\' "$_prompt_status"
    printf '\033]7;file://%s%s\033\\' "${HOSTNAME:-localhost}" "$PWD"
  }

  if [[ "$(declare -p PROMPT_COMMAND 2>/dev/null)" == "declare -a"* ]]; then
    PROMPT_COMMAND=(_prompt_precmd "${PROMPT_COMMAND[@]}")
  else
    PROMPT_COMMAND="_prompt_precmd${PROMPT_COMMAND:+;$PROMPT_COMMAND}"
  fi
fi
