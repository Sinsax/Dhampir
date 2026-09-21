# ASCII-only shim. PowerShell 5.1 parses .ps1 using the system ANSI code page,
# so non-ASCII text in this file would be a syntax error on a bare Windows box.
# All real logic (and every Chinese comment) lives in the .mjs next to it.
#
# Kept so that docs / CI blueprints that say "scripts/check-core-purity.ps1"
# keep working. Exit code is forwarded verbatim.

$ErrorActionPreference = 'Stop'

$node = Get-Command node -ErrorAction SilentlyContinue
if ($null -eq $node) {
    [Console]::Error.WriteLine('check-core-purity: node not found on PATH; refusing to report success.')
    exit 2
}

& node (Join-Path $PSScriptRoot 'check-core-purity.mjs') @args
exit $LASTEXITCODE
