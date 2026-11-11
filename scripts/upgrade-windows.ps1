$ErrorActionPreference = 'Stop'
& python (Join-Path $PSScriptRoot 'upgrade_local.py') win32 @args
exit $LASTEXITCODE
