# Offline failure-path tests. No Azure access, certificate import or signing.
. "$PSScriptRoot/signing.ps1"

$script:checks = 0
function Assert-Fails {
    param([scriptblock]$Body, [string]$Message)
    $caught = $false
    try { & $Body } catch {
        $caught = $true
        if ($_.Exception.Message -notlike "*$Message*") { throw }
    }
    if (-not $caught) { throw "Expected failure containing: $Message" }
    $script:checks++
}
function Assert-Passes {
    param([scriptblock]$Body)
    & $Body
    $script:checks++
}

$names = @('AZURE_TENANT_ID', 'AZURE_CLIENT_ID', 'AZURE_SUBSCRIPTION_ID',
    'AZURE_ENDPOINT', 'AZURE_CODE_SIGNING_NAME', 'AZURE_CERT_PROFILE_NAME')
$saved = @{}
$temp = Join-Path ([IO.Path]::GetTempPath()) ([guid]::NewGuid().ToString())
try {
    foreach ($name in $names) {
        $saved[$name] = [Environment]::GetEnvironmentVariable($name)
        [Environment]::SetEnvironmentVariable($name, 'test-value')
    }
    $env:AZURE_ENDPOINT = 'https://eus.codesigning.azure.net/'
    Assert-Passes { Assert-WindowsSigningConfiguration }
    foreach ($name in $names) {
        $value = [Environment]::GetEnvironmentVariable($name)
        [Environment]::SetEnvironmentVariable($name, ' ')
        Assert-Fails { Assert-WindowsSigningConfiguration } $name
        [Environment]::SetEnvironmentVariable($name, $value)
    }
    foreach ($endpoint in @('http://eus.codesigning.azure.net/',
        'https://evil.example/', 'https://eus.codesigning.azure.net.evil.example/',
        'https://user@eus.codesigning.azure.net/', 'https://eus.codesigning.azure.net:8443/',
        'https://eus.codesigning.azure.net/path', 'https://eus.codesigning.azure.net/?x=1')) {
        $env:AZURE_ENDPOINT = $endpoint
        Assert-Fails { Assert-WindowsSigningConfiguration } 'AZURE_ENDPOINT'
    }

    $null = New-Item -ItemType Directory -Path $temp
    if ($IsWindows) {
        $unsigned = Join-Path $temp 'unsigned.exe'
        Set-Content -LiteralPath $unsigned -Value 'unsigned fixture'
        Assert-Fails {
            Assert-WindowsSignature -Paths $unsigned -SignTool 'must-not-run'
        } 'Invalid Authenticode'
        Remove-Item -LiteralPath $unsigned
    }

    # Mock only the OS trust APIs and external SignTool. Exercise the real guards.
    $script:publisher = 'COINCUBE TECHNOLOGY LLC'
    $script:status = 'Valid'
    $script:timestamp = $true
    $script:signToolExit = 0
    $script:verified = @()
    function Get-AuthenticodeSignature {
        param([string]$LiteralPath)
        $cert = [pscustomobject]@{}
        $cert | Add-Member -MemberType ScriptMethod -Name GetNameInfo -Value {
            param($Type, $Issuer)
            return $script:publisher
        }
        return [pscustomobject]@{
            Status = $script:status
            SignerCertificate = $cert
            TimeStamperCertificate = $(if ($script:timestamp) { $cert } else { $null })
        }
    }
    function Test-SignTool {
        if (($args[0..4] -join ' ') -ne 'verify /pa /all /v /tw') {
            throw 'SignTool must verify all signatures, Authenticode trust and timestamps.'
        }
        $script:verified += $args[-1]
        $global:LASTEXITCODE = $script:signToolExit
    }
    $bin = New-Item -ItemType Directory -Path (Join-Path $temp 'COINCUBE/bin') -Force
    foreach ($name in @('coincube.exe', 'coincube-spark-bridge.exe', 'coincube-mcp.exe')) {
        Set-Content -LiteralPath (Join-Path $bin.FullName $name) -Value 'fixture'
    }
    $gui = Join-Path $bin.FullName 'coincube.exe'
    Assert-Passes { Assert-WindowsMsiPayload -Root $temp -SignTool Test-SignTool }
    if ($script:verified.Count -ne 3) { throw 'Must verify all three extracted executables.' }
    $msi = Join-Path $temp 'installer.msi'
    Set-Content -LiteralPath $msi -Value 'fixture'
    Assert-Passes { Assert-WindowsSignature -Paths $msi -SignTool Test-SignTool }
    foreach ($bad in @('NotSigned', 'HashMismatch', 'NotTrusted', 'UnknownError')) {
        $script:status = $bad
        Assert-Fails { Assert-WindowsSignature -Paths $gui -SignTool Test-SignTool } 'Invalid Authenticode'
    }
    $script:status = 'Valid'
    $script:publisher = 'OTHER COMPANY'
    Assert-Fails { Assert-WindowsSignature -Paths $gui -SignTool Test-SignTool } 'Unexpected publisher'
    $script:publisher = 'COINCUBE TECHNOLOGY LLC'
    $script:timestamp = $false
    Assert-Fails { Assert-WindowsSignature -Paths $gui -SignTool Test-SignTool } 'Missing timestamp'
    $script:timestamp = $true
    foreach ($code in @(1, 2)) {
        $script:signToolExit = $code
        Assert-Fails { Assert-WindowsSignature -Paths $gui -SignTool Test-SignTool } 'SignTool verification failed'
    }
    $script:signToolExit = 0
    Assert-Fails { Assert-WindowsSignature -Paths (Join-Path $temp 'missing.exe') -SignTool Test-SignTool } 'does not exist'
    Assert-Fails { Assert-WindowsSignature -Paths $temp -SignTool Test-SignTool } 'Not a file'
    $mcp = Join-Path $bin.FullName 'coincube-mcp.exe'
    Move-Item -LiteralPath $mcp -Destination $temp
    Assert-Fails { Assert-WindowsMsiPayload -Root $temp -SignTool Test-SignTool } 'same directory'
    Remove-Item -LiteralPath (Join-Path $temp 'coincube-mcp.exe')
    Assert-Fails { Assert-WindowsMsiPayload -Root $temp -SignTool Test-SignTool } 'exactly three executables'
    Set-Content -LiteralPath (Join-Path $bin.FullName 'unexpected.exe') -Value 'fixture'
    Assert-Fails { Assert-WindowsMsiPayload -Root $temp -SignTool Test-SignTool } 'exactly one coincube-mcp.exe'
    Set-Content -LiteralPath $mcp -Value 'fixture'
    Assert-Fails { Assert-WindowsMsiPayload -Root $temp -SignTool Test-SignTool } 'exactly three executables'
    Write-Host "PASS: $script:checks offline Windows signing checks"
} finally {
    foreach ($name in $names) {
        [Environment]::SetEnvironmentVariable($name, $saved[$name])
    }
    if (Test-Path -LiteralPath $temp) { Remove-Item -LiteralPath $temp -Recurse -Force }
}
# GitHub's pwsh wrapper propagates LASTEXITCODE, including from our failure mocks.
$global:LASTEXITCODE = 0
