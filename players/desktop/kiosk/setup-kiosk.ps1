<#
.SYNOPSIS
  Turns a Windows PC into a dedicated Qivomedia signage player.

.DESCRIPTION
  Run once, as administrator, after installing Qivomedia Player. It:
    1. writes the server address for every user of this PC (%ProgramData%\Qivomedia\Player\defaults.json)
    2. creates a local "signage" account and signs it in automatically at boot
    3. replaces that account's desktop with the player (Shell Launcher: no taskbar, no Start menu)
    4. never sleeps, never turns the screen off, hides the lock screen
    5. keeps Windows Update restarts outside 06:00–23:00
  Administrators still get the normal desktop.

  Shell Launcher needs Windows 10/11 Enterprise, Education or IoT Enterprise. On other editions
  the script skips step 3 and starts the player with a scheduled task when that account signs in.

.EXAMPLE
  .\setup-kiosk.ps1 -Server https://signage.univ.ma
.EXAMPLE
  .\setup-kiosk.ps1 -Server http://10.10.0.5:8080 -RestartAt 03:30
#>
#Requires -RunAsAdministrator
param(
  [Parameter(Mandatory)] [string] $Server,
  [string] $User = "signage",
  [string] $RestartAt = "04:00",
  [string] $Player = "$env:ProgramFiles\Qivomedia Player\qivo-player.exe"
)
$ErrorActionPreference = "Stop"
function Step($t) { Write-Host "`n==> $t" -ForegroundColor Cyan }

if (-not (Test-Path $Player)) { throw "Qivomedia Player not found at '$Player'. Install it first, or pass -Player <path>." }
if ($Server -notmatch '^https?://') { $Server = "https://$Server" }

Step "Server address for every user: $Server"
$defaults = Join-Path $env:ProgramData "Qivomedia\Player"
New-Item -ItemType Directory -Force $defaults | Out-Null
@{ server_url = $Server.TrimEnd('/'); restart_at = $RestartAt; autostart = $true } | ConvertTo-Json | Set-Content -Encoding UTF8 (Join-Path $defaults "defaults.json")

Step "Local account '$User'"
$plain = -join ((48..57 + 65..90 + 97..122) | Get-Random -Count 24 | ForEach-Object { [char]$_ })
$secure = ConvertTo-SecureString $plain -AsPlainText -Force
if (Get-LocalUser -Name $User -ErrorAction SilentlyContinue) {
  Set-LocalUser -Name $User -Password $secure -PasswordNeverExpires $true
} else {
  New-LocalUser -Name $User -Password $secure -FullName "Qivomedia signage" -Description "Runs the Qivomedia player" -PasswordNeverExpires -UserMayNotChangePassword | Out-Null
  Add-LocalGroupMember -Group "Users" -Member $User
}

Step "Automatic sign-in at boot"
# Stored by Windows' own Winlogon settings. For an encrypted copy, use Sysinternals Autologon instead.
$wl = "HKLM:\SOFTWARE\Microsoft\Windows NT\CurrentVersion\Winlogon"
Set-ItemProperty $wl AutoAdminLogon "1"
Set-ItemProperty $wl DefaultUserName $User
Set-ItemProperty $wl DefaultDomainName $env:COMPUTERNAME
Set-ItemProperty $wl DefaultPassword $plain
Remove-ItemProperty $wl AutoLogonCount -ErrorAction SilentlyContinue

Step "Player instead of the desktop for '$User' (Shell Launcher)"
$edition = (Get-CimInstance Win32_OperatingSystem).Caption
$feature = Get-WindowsOptionalFeature -Online -FeatureName Client-EmbeddedShellLauncher -ErrorAction SilentlyContinue
if ($feature) {
  if ($feature.State -ne "Enabled") {
    Enable-WindowsOptionalFeature -Online -FeatureName Client-EmbeddedShellLauncher -All -NoRestart | Out-Null
  }
  try {
    $sl = [wmiclass]"\\localhost\root\standardcimv2\embedded:WESL_UserSetting"
    $sid = (New-Object System.Security.Principal.NTAccount($User)).Translate([System.Security.Principal.SecurityIdentifier]).Value
    try { $sl.RemoveCustomShell($sid) | Out-Null } catch {}
    # Exit code 0 (a technician chose "Exit player"): leave it closed. Anything else: restart the player.
    $sl.SetCustomShell($sid, "`"$Player`"", [int[]]@(0), [int[]]@(3), 0) | Out-Null
    $sl.SetDefaultShell("explorer.exe", 0) | Out-Null
    $sl.SetEnabled($true) | Out-Null
    Write-Host "    '$User' now boots straight into the player."
  } catch {
    Write-Warning "Shell Launcher was just enabled: restart the PC and run this script again to finish this step."
  }
} else {
  Write-Warning "Shell Launcher isn't available on '$edition'. The player will start when '$User' signs in instead (the taskbar stays behind it)."
  $action = New-ScheduledTaskAction -Execute $Player
  $trigger = New-ScheduledTaskTrigger -AtLogOn -User $User
  $principal = New-ScheduledTaskPrincipal -UserId $User -LogonType Interactive -RunLevel Limited
  $settings = New-ScheduledTaskSettingsSet -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries -ExecutionTimeLimit ([TimeSpan]::Zero)
  Register-ScheduledTask -TaskName "Qivomedia Player" -Action $action -Trigger $trigger -Principal $principal -Settings $settings -Force | Out-Null
}

Step "Never sleep, never turn the screen off"
powercfg /change monitor-timeout-ac 0
powercfg /change standby-timeout-ac 0
powercfg /change hibernate-timeout-ac 0
powercfg /hibernate off
$p = "HKLM:\SOFTWARE\Policies\Microsoft\Windows\Personalization"
New-Item -Force $p | Out-Null
Set-ItemProperty $p NoLockScreen 1 -Type DWord

Step "Windows Update: no restarts between 06:00 and 23:00"
$wu = "HKLM:\SOFTWARE\Policies\Microsoft\Windows\WindowsUpdate"
New-Item -Force $wu | Out-Null
Set-ItemProperty $wu SetActiveHours 1 -Type DWord
Set-ItemProperty $wu ActiveHoursStart 6 -Type DWord
Set-ItemProperty $wu ActiveHoursEnd 23 -Type DWord

Write-Host "`nDone. Restart the PC: it will sign in as '$User' and show the pairing code." -ForegroundColor Green
Write-Host "To reach the desktop later: Ctrl+Alt+Del > Sign out, then sign in with an administrator account."
