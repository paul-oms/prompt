# Prompt shell integration for zsh.
#
# Loaded from Prompt's managed ZDOTDIR shim. It never modifies the user's
# dotfiles: the shim sources them first, then adds two hooks that tell the
# terminal where prompts and commands begin. Prompt uses those markers to know
# when the editor should own the keyboard.

if [[ "${PROMPT_SHELL_INTEGRATION:-0}" == "1" ]]; then
  export PROMPT_SHELL_INTEGRATION=0

  autoload -Uz add-zsh-hook 2>/dev/null || true

  _prompt_emit_pwd() {
    builtin printf '\033]7;file://%s%s\033\\' "${HOST:-localhost}" "${PWD}"
  }

  _prompt_precmd() {
    builtin local _prompt_status=$?
    builtin printf '\033]133;D;%d\033\\\033]133;A\033\\' "$_prompt_status"
    _prompt_emit_pwd
  }

  _prompt_preexec() {
    builtin printf '\033]133;C\033\\'
  }

  if (( ${+functions[add-zsh-hook]} )); then
    add-zsh-hook precmd _prompt_precmd
    add-zsh-hook preexec _prompt_preexec
    add-zsh-hook chpwd _prompt_emit_pwd
  else
    precmd_functions+=(_prompt_precmd)
    preexec_functions+=(_prompt_preexec)
    chpwd_functions+=(_prompt_emit_pwd)
  fi
fi
