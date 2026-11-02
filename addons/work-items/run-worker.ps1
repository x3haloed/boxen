param([Parameter(Mandatory)][string]$Executable,[Parameter(Mandatory)][string]$DataDirectory,[Parameter(Mandatory)][string]$Log)
$ErrorActionPreference='Stop'
New-Item -ItemType Directory -Force (Split-Path -Parent $Log) | Out-Null
if((Test-Path -LiteralPath $Log) -and (Get-Item -LiteralPath $Log).Length -gt 10MB){Move-Item -LiteralPath $Log -Destination ($Log+'.1') -Force}
& $Executable --home $DataDirectory run *>> $Log
exit $LASTEXITCODE
