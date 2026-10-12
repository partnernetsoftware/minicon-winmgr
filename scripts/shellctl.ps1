<#
.SYNOPSIS
  在 Windows explorer / 备用外壳 / minicon-winmgr 之间查看、启动、切换外壳。
.DESCRIPTION
  - 注册表改动一律委托给 minicon-winmgr.exe（--shell-*），写入需 -Confirm。
  - 进程启停由本脚本负责（Start-Process / Stop-Process）。
  - explorer 视为"默认"（清空 Shell 值），不会被本脚本停止。
.EXAMPLE
  .\scripts\shellctl.ps1 status
  .\scripts\shellctl.ps1 list
  .\scripts\shellctl.ps1 start -Target minicon
  .\scripts\shellctl.ps1 stop  -Target minicon
  .\scripts\shellctl.ps1 switch -Target alt -Confirm
#>
[CmdletBinding()]
param(
  [Parameter(Mandatory = $true, Position = 0)]
  [ValidateSet('status', 'list', 'set', 'start', 'stop', 'restart', 'switch', 'help')]
  [string]$Action,

  [Parameter(Position = 1)]
  [ValidateSet('explorer', 'alt', 'minicon')]
  [string]$Target,

  [switch]$Confirm
)

$ErrorActionPreference = 'Stop'

$RepoDir    = Split-Path -Parent $PSScriptRoot
$MiniconExe = Join-Path $RepoDir 'target\debug\minicon-winmgr.exe'
if (-not (Test-Path $MiniconExe)) { $MiniconExe = Join-Path $RepoDir 'target\release\minicon-winmgr.exe' }

# 备用外壳路径来自环境变量 MINICON_ALT_SHELL，未设置时该 profile 不可用（不内置任何默认路径）。
$AltExe  = if ($env:MINICON_ALT_SHELL) { $env:MINICON_ALT_SHELL } else { $null }
$AltProc = if ($AltExe) { [IO.Path]::GetFileNameWithoutExtension($AltExe) } else { $null }

$Profiles = [ordered]@{
  explorer = @{ Label = 'Windows explorer (默认)'; Exe = $null;       Process = 'explorer' }
  alt      = @{ Label = '备用外壳';                Exe = $AltExe;     Process = $AltProc }
  minicon  = @{ Label = 'minicon-winmgr';          Exe = $MiniconExe; Process = 'minicon-winmgr' }
}

function Get-MiniconExe {
  if (-not (Test-Path $MiniconExe)) {
    throw "未找到 minicon-winmgr.exe，请先 cargo build（查找路径: $MiniconExe）"
  }
  return $MiniconExe
}

function Invoke-MiniconRegistry {
  param(
    [Parameter(Mandatory)][ValidateSet('status', 'list', 'set')][string]$Verb,
    [string]$Arg
  )
  $exe = Get-MiniconExe
  switch ($Verb) {
    'status' { & $exe --shell-status; return }
    'list'   { & $exe --shell-list;   return }
    'set'    {
      $env:MINICON_CONFIRM_SHELL = 'yes'
      try { & $exe --shell-set $Arg } finally { Remove-Item Env:MINICON_CONFIRM_SHELL -ErrorAction SilentlyContinue }
      return
    }
  }
}

function Test-ShellRunning {
  param([string]$Name)
  $proc = $Profiles[$Name].Process
  if (-not $proc) { return $false }
  [bool](Get-Process -Name $proc -ErrorAction SilentlyContinue)
}

function Start-Shell {
  param([Parameter(Mandatory)][string]$Name)
  if (Test-ShellRunning $Name) { Write-Host "[$Name] 已在运行"; return }
  $p = $Profiles[$Name]
  if ($Name -eq 'explorer') {
    Start-Process 'explorer.exe'
    Write-Host "[explorer] 已启动"
    return
  }
  if (-not $p.Exe) { throw "[$Name] 未配置可执行文件（请设置 `$env:MINICON_ALT_SHELL）" }
  if (-not (Test-Path $p.Exe)) { throw "[$Name] 可执行文件不存在: $($p.Exe)" }
  Start-Process -FilePath $p.Exe
  Write-Host "[$Name] 已启动: $($p.Exe)"
}

function Stop-Shell {
  param([Parameter(Mandatory)][string]$Name)
  if ($Name -eq 'explorer') { throw "拒绝停止 explorer.exe（会关掉桌面/任务栏），如需请手动处理" }
  $proc = $Profiles[$Name].Process
  if (-not $proc) { throw "[$Name] 未配置进程名（请设置 `$env:MINICON_ALT_SHELL）" }
  $procs = @(Get-Process -Name $proc -ErrorAction SilentlyContinue)
  if ($procs.Count -eq 0) { Write-Host "[$Name] 未在运行"; return }
  $procs | Stop-Process -Force
  Write-Host "[$Name] 已停止（$($procs.Count) 个进程）"
}

switch ($Action) {
  'status' { Invoke-MiniconRegistry status }
  'list'   { Invoke-MiniconRegistry list }
  'set'    {
    if (-not $Target) { throw 'set 需要 -Target <explorer|alt|minicon>' }
    if (-not $Confirm) { throw 'set 会修改 Winlogon Shell 注册表，需加 -Confirm 才执行' }
    Invoke-MiniconRegistry set $Target
  }
  'start' {
    if (-not $Target) { throw 'start 需要 -Target <explorer|alt|minicon>' }
    Start-Shell $Target
  }
  'stop' {
    if (-not $Target) { throw 'stop 需要 -Target <alt|minicon>' }
    Stop-Shell $Target
  }
  'restart' {
    if (-not $Target) { throw 'restart 需要 -Target <explorer|alt|minicon>' }
    if ($Target -ne 'explorer') { Stop-Shell $Target }
    Start-Shell $Target
  }
  'switch' {
    if (-not $Target) { throw 'switch 需要 -Target <explorer|alt|minicon>' }
    if (-not $Confirm) { throw 'switch 会修改 Winlogon Shell 注册表并启停外壳，需加 -Confirm 才执行' }
    foreach ($k in $Profiles.Keys) {
      if ($k -ne $Target -and $k -ne 'explorer' -and $Profiles[$k].Exe) { Stop-Shell $k }
    }
    Invoke-MiniconRegistry set $Target
    Start-Shell $Target
    Write-Host "[switch] 当前外壳 -> $Target（作为登录外壳需注销/重新登录后生效）"
  }
  'help' { Get-Help $PSCommandPath -Detailed }
}
