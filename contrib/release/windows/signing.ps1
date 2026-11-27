# Dot-source this file from the Windows release jobs and offline regression tests.
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

function Assert-WindowsSigningConfiguration {
    foreach ($name in @(
        'AZURE_TENANT_ID', 'AZURE_CLIENT_ID', 'AZURE_SUBSCRIPTION_ID',
        'AZURE_ENDPOINT', 'AZURE_CODE_SIGNING_NAME', 'AZURE_CERT_PROFILE_NAME'
    )) {
        if ([string]::IsNullOrWhiteSpace([Environment]::GetEnvironmentVariable($name))) {
            throw "Missing Windows signing configuration: $name. See docs/WINDOWS_CODE_SIGNING.md."
        }
    }
    $endpoint = [uri]$env:AZURE_ENDPOINT
    if ($endpoint.Scheme -ne 'https' -or
        $endpoint.Host -notmatch '^[a-z0-9-]+\.codesigning\.azure\.net$' -or
        -not $endpoint.IsDefaultPort -or $endpoint.UserInfo -or
        $endpoint.AbsolutePath -ne '/' -or $endpoint.Query -or $endpoint.Fragment) {
        throw 'AZURE_ENDPOINT must be the HTTPS regional Artifact Signing endpoint.'
    }
}

function Get-WindowsSignTool {
    # Use the hosted runner's SDK, not a file in the Cargo cache or checkout.
    $sdk = Join-Path ${env:ProgramFiles(x86)} 'Windows Kits/10/bin'
    $tool = Get-ChildItem -Path "$sdk/*/x64/signtool.exe" -File |
        Sort-Object { [version]$_.Directory.Parent.Name } -Descending |
        Select-Object -First 1
    if (-not $tool) { throw 'Windows SDK x64 signtool.exe was not found.' }
    return $tool.FullName
}

function Assert-WindowsSignature {
    param(
        [Parameter(Mandatory)][string[]]$Paths,
        [string]$SignTool = (Get-WindowsSignTool)
    )
    if ($Paths.Count -eq 0) { throw 'No files supplied for signature verification.' }
    foreach ($path in $Paths) {
        $file = Get-Item -LiteralPath $path
        if ($file.PSIsContainer) { throw "Not a file: $path" }
        $signature = Get-AuthenticodeSignature -LiteralPath $file.FullName
        if ($signature.Status -ne 'Valid' -or -not $signature.SignerCertificate) {
            throw "Invalid Authenticode signature: $path ($($signature.Status))."
        }
        $publisher = $signature.SignerCertificate.GetNameInfo(
            [System.Security.Cryptography.X509Certificates.X509NameType]::SimpleName, $false)
        if ($publisher -cne 'COINCUBE TECHNOLOGY LLC') {
            throw "Unexpected publisher for ${path}: $publisher."
        }
        if (-not $signature.TimeStamperCertificate) {
            throw "Missing timestamp: $path."
        }
        & $SignTool verify /pa /all /v /tw $file.FullName
        if ($LASTEXITCODE -ne 0) {
            throw "SignTool verification failed for $path (exit $LASTEXITCODE)."
        }
    }
}

function Assert-WindowsStagedFiles {
    # Artifacts cross job boundaries, so accept only the exact expected files.
    param(
        [Parameter(Mandatory)][string]$Root,
        [Parameter(Mandatory)][string[]]$Names
    )
    $items = @(Get-ChildItem -LiteralPath $Root -Recurse -Force)
    if ($items.Count -eq 0) { throw "Unexpected staged files: $Root is empty." }
    foreach ($item in $items) {
        if ($item.PSIsContainer) { throw "Unexpected directory in staged files: $($item.Name)." }
        if ($item.LinkType) { throw "Unexpected link in staged files: $($item.Name)." }
    }
    $actual = @($items.Name | Sort-Object -CaseSensitive)
    $wanted = @($Names | Sort-Object -CaseSensitive)
    if (($actual -join "`n") -cne ($wanted -join "`n")) {
        throw "Unexpected staged files: found [$($actual -join ', ')], expected [$($wanted -join ', ')]."
    }
}

function Get-WindowsStagedDigest {
    param([Parameter(Mandatory)][string]$Root)
    $files = @(Get-ChildItem -LiteralPath $Root -File -Force | Sort-Object Name -CaseSensitive)
    if ($files.Count -eq 0) { throw "No staged files in $Root." }
    return (@($files | ForEach-Object {
        "$($_.Name)=$((Get-FileHash -LiteralPath $_.FullName -Algorithm SHA256).Hash.ToLowerInvariant())"
    }) -join ',')
}

function Assert-WindowsStagedDigest {
    param(
        [Parameter(Mandatory)][string]$Root,
        [AllowEmptyString()][string]$Expected
    )
    if ([string]::IsNullOrWhiteSpace($Expected)) { throw 'Missing expected staged-file digest.' }
    $actual = Get-WindowsStagedDigest -Root $Root
    if ($actual -cne $Expected) {
        throw "Staged file digest mismatch: found $actual, expected $Expected."
    }
}

function Assert-WindowsMsiPayload {
    param(
        [Parameter(Mandatory)][string]$Root,
        [string]$SignTool = (Get-WindowsSignTool)
    )
    $expected = @('coincube.exe', 'coincube-spark-bridge.exe', 'coincube-mcp.exe')
    $files = @(Get-ChildItem -LiteralPath $Root -Recurse -File -Filter '*.exe')
    if ($files.Count -ne $expected.Count) {
        throw "Expected exactly three executables in the MSI, found $($files.Count)."
    }
    foreach ($name in $expected) {
        if (@($files | Where-Object Name -eq $name).Count -ne 1) {
            throw "Expected exactly one $name in the MSI."
        }
    }
    if (@($files.DirectoryName | Select-Object -Unique).Count -ne 1) {
        throw 'All three MSI executables must install into the same directory.'
    }
    Assert-WindowsSignature -Paths $files.FullName -SignTool $SignTool
}
