# Letronna for Windows.
#
#   iex (irm https://letronna.ai/install.ps1)
#
# This only delivers the program. It downloads the latest release into a
# temporary folder and hands over to `letronna setup`, which asks your name,
# signs you in, picks a provider and a model, and puts the window and the
# terminal where you choose. Nothing needs administrator rights, and nothing
# goes outside your own account.
#
# LETRONNA_DOWNLOAD  a different archive, as a URL or a local path
# LETRONNA_NO_SETUP  set to anything to unpack and stop, for unattended use

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'   # the progress bar makes downloads several times slower

$source = if ($env:LETRONNA_DOWNLOAD) {
    $env:LETRONNA_DOWNLOAD
} else {
    'https://github.com/gadlus/letronna/releases/latest/download/letronna-windows-x64.zip'
}

Write-Host ''
Write-Host '  Letronna' -ForegroundColor Blue -NoNewline
Write-Host '  fetching the latest release' -ForegroundColor DarkGray

$staging = Join-Path ([IO.Path]::GetTempPath()) ("letronna-" + [Guid]::NewGuid().ToString('N').Substring(0, 8))
New-Item -ItemType Directory -Path $staging | Out-Null
$archive = Join-Path $staging 'letronna.zip'

try {
    if (Test-Path $source) {
        Copy-Item $source $archive
    } else {
        Invoke-WebRequest -Uri $source -OutFile $archive -UseBasicParsing
    }
} catch {
    Write-Host "  Could not download $source" -ForegroundColor Red
    Write-Host "  $($_.Exception.Message)" -ForegroundColor DarkGray
    exit 1
}

Expand-Archive -Path $archive -DestinationPath $staging -Force
$exe = Get-ChildItem -Path $staging -Recurse -Filter 'letronna.exe' | Select-Object -First 1
if (-not $exe) {
    Write-Host '  The download did not contain letronna.exe.' -ForegroundColor Red
    exit 1
}

if ($env:LETRONNA_NO_SETUP) {
    Write-Host "  Unpacked to $($exe.DirectoryName)" -ForegroundColor DarkGray
    exit 0
}

# Setup asks questions, so it runs in this console, in the foreground. It
# copies itself into place, so the temporary copy can go when it is done.
& $exe.FullName setup
$code = $LASTEXITCODE
Remove-Item -Recurse -Force $staging -ErrorAction SilentlyContinue
exit $code
