param(
    [Parameter(Mandatory)][string]$ConfigFile,
    [Parameter(Mandatory)][string]$LegacyDatabase,
    [string]$DataDirectory=(Join-Path $env:USERPROFILE '.boxen-work-items'),
    [switch]$NoBuild
)
$ErrorActionPreference='Stop'
$runtimeHome=[IO.Path]::GetFullPath($DataDirectory)
$bin=Join-Path $runtimeHome 'bin'
$exe=Join-Path $bin 'boxen-work-items.exe'
$wrapper=Join-Path $bin 'run-worker.ps1'
$taskName='Boxen-Work-Items'
if(!$NoBuild){& python (Join-Path $PSScriptRoot '../../scripts/build-work-items.py');if($LASTEXITCODE -ne 0){throw 'Build failed'}}
$old=Get-ScheduledTask -TaskName $taskName -ErrorAction SilentlyContinue
if($old){
    if(($old.Actions.Arguments -join ' ') -notlike ('*'+$runtimeHome+'*')){throw 'Existing task belongs to another service home'}
    Stop-ScheduledTask -TaskName $taskName
}
Get-CimInstance Win32_Process -Filter "Name='boxen-work-items.exe'" | Where-Object {$_.ExecutablePath -eq $exe -and $_.CommandLine.Contains($runtimeHome)} | ForEach-Object {Stop-Process -Id $_.ProcessId}
New-Item -ItemType Directory -Force $bin | Out-Null
Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'target/release/boxen-work-items.exe') -Destination $exe -Force
Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'run-worker.ps1') -Destination $wrapper -Force
& $exe --home $runtimeHome init --config-file ([IO.Path]::GetFullPath($ConfigFile)) --legacy-db ([IO.Path]::GetFullPath($LegacyDatabase))
if($LASTEXITCODE -ne 0){throw 'Initialization failed; original mirror/records are retained for diagnosis'}
$powershell=Join-Path $env:SystemRoot 'System32/WindowsPowerShell/v1.0/powershell.exe'
$log=Join-Path $runtimeHome 'logs/service.jsonl'
$arguments='-NoProfile -NonInteractive -WindowStyle Hidden -ExecutionPolicy Bypass -File "'+$wrapper+'" -Executable "'+$exe+'" -DataDirectory "'+$runtimeHome+'" -Log "'+$log+'"'
$userName=[Security.Principal.WindowsIdentity]::GetCurrent().Name
$action=New-ScheduledTaskAction -Execute $powershell -Argument $arguments
$trigger=New-ScheduledTaskTrigger -AtLogOn -User $userName
$principal=New-ScheduledTaskPrincipal -UserId $userName -LogonType Interactive -RunLevel Limited
$settings=New-ScheduledTaskSettingsSet -StartWhenAvailable -MultipleInstances IgnoreNew -ExecutionTimeLimit ([TimeSpan]::Zero) -RestartCount 3 -RestartInterval (New-TimeSpan -Minutes 1) -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries
Register-ScheduledTask -TaskName $taskName -Action $action -Trigger $trigger -Principal $principal -Settings $settings -Description 'Durable Azure mirror and supervised Boxen coder assignments; optional operator-configured automatic development-to-PR workflow.' -Force | Out-Null
Start-ScheduledTask -TaskName $taskName
[pscustomobject]@{task=$taskName;executable=$exe;home=$runtimeHome;log=$log} | ConvertTo-Json
