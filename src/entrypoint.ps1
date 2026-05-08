# arbox Windows container entrypoint
#
# Stages .claude.json through the mounted .claude directory to work around Docker's
# lack of support for binding individual files on Windows containers.
#
# The relay flow:
#   1. Rust stages host .claude.json → .claude\.claude.json (before launch)
#   2. This entrypoint copies .claude\.claude.json → $HOME\.claude.json (at startup)
#   3. The requested command runs and may modify .claude.json
#   4. This entrypoint copies $HOME\.claude.json → .claude\.claude.json (on exit)
#   5. Rust copies .claude\.claude.json → host .claude.json (after container exits)
#
# Result: credential changes inside the container persist to the host.

$json  = "$env:HOME\.claude.json"
$stash = "$env:HOME\.claude\.claude.json"

# Copy staged credentials into container home
if (Test-Path $stash) { Copy-Item $stash $json -Force }

# Run the requested command, forwarding all args
if ($args.Count -eq 0) {
    exit 1
}

$cmd  = $args[0]
$rest = if ($args.Count -gt 1) { , @($args[1..($args.Count - 1)]) } else { @() }

# If cmd looks like an executable path or a known Windows command, run it directly.
# Otherwise, assume it's a bash command and run it through bash.
if ((Test-Path $cmd) -or ($cmd -match '\\' -or $cmd -match '\.exe$' -or $cmd -match '^(powershell|cmd|bash|pwsh)')) {
    & $cmd @rest
} else {
    # Run through bash (common case for arbox run)
    & 'C:\Program Files\Git\bin\bash.exe' -l -c ($cmd + ' ' + ($rest -join ' '))
}
$code = $LASTEXITCODE

# Sync any credential changes back to the mounted .claude dir
if (Test-Path $json) { Copy-Item $json $stash -Force }

exit $code
