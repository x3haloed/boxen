param([Parameter(Mandatory)][string]$Executable,[Parameter(Mandatory)][string]$Config,[Parameter(Mandatory)][string]$Log)
$ErrorActionPreference='Stop'
$folder=Split-Path -Parent $Log
New-Item -ItemType Directory -Force -Path $folder | Out-Null
if((Test-Path -LiteralPath $Log) -and (Get-Item -LiteralPath $Log).Length -gt 10MB){
    Move-Item -LiteralPath $Log -Destination ($Log+'.1') -Force
}
& $Executable --config $Config run *>> $Log
exit $LASTEXITCODE
