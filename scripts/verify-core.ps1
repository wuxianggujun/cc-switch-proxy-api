[CmdletBinding()]
param([switch]$Full)

$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = [System.Text.UTF8Encoding]::new($false)
$OutputEncoding = [Console]::OutputEncoding

$repository = Split-Path -Parent $PSScriptRoot
$originalLocation = Get-Location
$names = @('PATH', 'CC_SWITCH_TEST_HOME', 'NO_PROXY', 'CARGO_INCREMENTAL')
$saved = @{}
foreach ($name in $names) {
    $saved[$name] = [Environment]::GetEnvironmentVariable($name, 'Process')
}
$testHome = Join-Path ([IO.Path]::GetTempPath()) ('cc-switch-verify-' + [guid]::NewGuid().ToString('N'))
[IO.Directory]::CreateDirectory($testHome) | Out-Null

try {
    Set-Location -LiteralPath $repository
    $cargoBin = Join-Path $env:USERPROFILE '.cargo\bin'
    if (Test-Path -LiteralPath $cargoBin -PathType Container) {
        $env:PATH = $cargoBin + ';' + $env:PATH
    }
    $env:CC_SWITCH_TEST_HOME = $testHome
    $env:NO_PROXY = '127.0.0.1,localhost,::1'
    if ($saved['NO_PROXY']) { $env:NO_PROXY += ',' + $saved['NO_PROXY'] }
    $env:CARGO_INCREMENTAL = '0'

    $testArgs = @('--test-threads=1', '--format=terse')
    if (-not $Full) {
        $testArgs += @('api_gateway', 'gateway_http', 'proxy_pool', 'checkin', 'explicit_test_home')
    }
    & cargo.exe test --manifest-path (Join-Path $repository 'src-tauri\Cargo.toml') --lib --locked --config 'profile.test.package.cc-switch.debug=0' -- @testArgs
    if ($LASTEXITCODE -ne 0) { throw "Rust 测试失败，退出码 $LASTEXITCODE" }

    & pnpm.cmd typecheck
    if ($LASTEXITCODE -ne 0) { throw "TypeScript 检查失败，退出码 $LASTEXITCODE" }
    if ($Full) {
        & pnpm.cmd test:unit --maxWorkers=2 --minWorkers=1
    } else {
        & pnpm.cmd test:unit tests/components/GatewayStatusBar.test.tsx tests/config/localeCoverage.test.ts --maxWorkers=2 --minWorkers=1
    }
    if ($LASTEXITCODE -ne 0) { throw "前端测试失败，退出码 $LASTEXITCODE" }
    Write-Host '验证通过。CC_SWITCH_TEST_HOME 已指定独立的测试配置目录。'
}
finally {
    foreach ($name in $names) {
        [Environment]::SetEnvironmentVariable($name, $saved[$name], 'Process')
    }
    Set-Location -LiteralPath $originalLocation
    Write-Host "隔离测试目录（保留供排查）：$testHome"
}
