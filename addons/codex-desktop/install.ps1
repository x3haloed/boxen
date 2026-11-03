param(
    [Parameter(Mandatory)][string]$ThreadId,
    [string]$Label='support',
    [string]$InstallRoot=(Join-Path $env:USERPROFILE '.boxen-support'),
    [string]$BoxenHome=(Join-Path $env:USERPROFILE '.boxen'),
    [string]$Url='http://127.0.0.1:42425',
    [string]$CodexBinary,
    [switch]$NoBuild
)
$ErrorActionPreference='Stop'
if($Label -notmatch '^[a-z0-9][a-z0-9-]{0,63}$'){throw 'The installed profile label uses lowercase letters, digits and hyphens.'}
$installBase=[IO.Path]::GetFullPath($InstallRoot)
$profile=Join-Path $installBase $Label
$bin=Join-Path $installBase 'bin'
$config=Join-Path $profile 'config.json'
$exe=Join-Path $bin 'boxen-support.exe'
$worker=Join-Path $bin 'run-worker.ps1'
$taskName='Boxen-Codex-Desktop-'+$Label
if(!$NoBuild){
    & cargo build --release --locked --manifest-path (Join-Path $PSScriptRoot 'Cargo.toml')
    if($LASTEXITCODE -ne 0){throw 'Adapter build failed'}
}
if(!$CodexBinary){$CodexBinary='auto'}
if($CodexBinary -ne 'auto'){$CodexBinary=[IO.Path]::GetFullPath($CodexBinary)}
$built=Join-Path $PSScriptRoot 'target\release\boxen-support.exe'
if(!(Test-Path -LiteralPath $built)){throw 'The release executable is unavailable'}
New-Item -ItemType Directory -Force -Path $bin,$profile | Out-Null
$old=Get-ScheduledTask -TaskName $taskName -ErrorAction SilentlyContinue
if($old){
    if($old.Actions.Execute -notcontains (Join-Path $env:SystemRoot 'System32\WindowsPowerShell\v1.0\powershell.exe')){throw 'An unrelated scheduled task already uses this name'}
    if(($old.Actions.Arguments -join ' ') -notlike ('*'+$config+'*')){throw 'The existing task refers to another profile'}
    Stop-ScheduledTask -TaskName $taskName
}
# Only this installed adapter executable is stopped for an upgrade.
Get-CimInstance Win32_Process -Filter "Name='boxen-support.exe'" | Where-Object {$_.ExecutablePath -eq $exe -and $_.CommandLine.Contains($config)} | ForEach-Object {Stop-Process -Id $_.ProcessId -ErrorAction Stop}
Copy-Item -LiteralPath $built -Destination $exe -Force
Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'run-worker.ps1') -Destination $worker -Force
& $exe --config $config init $ThreadId --codex $CodexBinary --boxen-home ([IO.Path]::GetFullPath($BoxenHome)) --url $Url --label $Label
if($LASTEXITCODE -ne 0){throw 'Adapter configuration failed'}
$powershell=Join-Path $env:SystemRoot 'System32\WindowsPowerShell\v1.0\powershell.exe'
$log=Join-Path $profile 'logs\bridge.jsonl'
$arguments='-NoProfile -NonInteractive -WindowStyle Hidden -ExecutionPolicy Bypass -File "'+$worker+'" -Executable "'+$exe+'" -Config "'+$config+'" -Log "'+$log+'"'
$action=New-ScheduledTaskAction -Execute $powershell -Argument $arguments
$trigger=New-ScheduledTaskTrigger -AtLogOn -User ([Security.Principal.WindowsIdentity]::GetCurrent().Name)
$principal=New-ScheduledTaskPrincipal -UserId ([Security.Principal.WindowsIdentity]::GetCurrent().Name) -LogonType Interactive -RunLevel Limited
$settings=New-ScheduledTaskSettingsSet -StartWhenAvailable -MultipleInstances IgnoreNew -ExecutionTimeLimit ([TimeSpan]::Zero) -RestartCount 3 -RestartInterval (New-TimeSpan -Minutes 1)
Register-ScheduledTask -TaskName $taskName -Action $action -Trigger $trigger -Principal $principal -Settings $settings -Description 'External Boxen client-mail adapter; the Codex desktop thread keeps its own execution owner.' -Force | Out-Null
Start-ScheduledTask -TaskName $taskName
[pscustomobject]@{task=$taskName;executable=$exe;config=$config;log=$log} | ConvertTo-Json
