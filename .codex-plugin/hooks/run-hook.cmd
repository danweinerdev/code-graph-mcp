: <<'BATCH_SECTION'
@echo off
setlocal EnableDelayedExpansion
rem ---------------------------------------------------------------------------
rem code-graph hook dispatcher (dual-interpreter file).
rem
rem   cmd.exe  reads the batch block below and re-invokes the named sibling hook
rem            script under whatever bash it can find.
rem   bash     treats the opening `:` as a no-op whose heredoc swallows the whole
rem            batch block, then runs the shell section at the bottom.
rem
rem Hook scripts dispatched through here carry no file extension on purpose:
rem Claude Code's Windows shim rewrites any command containing ".sh" to run under
rem bash, which would double-wrap the invocation.
rem
rem   run-hook.cmd <hook-name> [args...]
rem ---------------------------------------------------------------------------

if "%~1"=="" (
    >&2 echo run-hook.cmd: no hook name given
    exit /b 1
)

set "CG_HOOK_DIR=%~dp0"
set "CG_HOOK=%~1"
shift

rem Re-quote the remaining arguments one at a time. A fixed %2..%9 expansion
rem would silently drop everything past the ninth argument.
set "CG_ARGS="
:gather
if "%~1"=="" goto dispatch
set "CG_ARGS=!CG_ARGS! "%~1""
shift
goto gather

:dispatch
for %%B in (
    "%ProgramFiles%\Git\bin\bash.exe"
    "%ProgramFiles(x86)%\Git\bin\bash.exe"
    "%LOCALAPPDATA%\Programs\Git\bin\bash.exe"
) do (
    if exist %%B (
        %%B "!CG_HOOK_DIR!!CG_HOOK!"!CG_ARGS!
        exit /b !ERRORLEVEL!
    )
)

where bash >nul 2>nul && (
    bash "!CG_HOOK_DIR!!CG_HOOK!"!CG_ARGS!
    exit /b !ERRORLEVEL!
)

rem No bash on this machine. Exit clean: hooks are advisory, and a missing
rem interpreter must never fail the session or the tool call that triggered us.
exit /b 0
BATCH_SECTION

# --- POSIX section -----------------------------------------------------------
# Reached only under bash/sh; cmd.exe never gets here.

if [ "$#" -eq 0 ]; then
	printf 'run-hook.cmd: no hook name given\n' >&2
	exit 1
fi

hook_dir="$(cd -- "$(dirname -- "$0")" && pwd)"
hook_name="$1"
shift

if [ ! -f "$hook_dir/$hook_name" ]; then
	printf 'run-hook.cmd: no such hook: %s\n' "$hook_dir/$hook_name" >&2
	exit 1
fi

exec bash "$hook_dir/$hook_name" "$@"
