# Letronna releases, from this machine.
#
#   scripts/release.ps1 build                build Windows here, into a draft
#   scripts/release.ps1 sign v0.1.0-alpha.2  sign a draft and publish it
#
# A v* tag makes CI build every platform into a draft release. Signing never
# happens there: the key every copy of Letronna trusts stays on the machine
# that runs `sign`, so a workflow or a GitHub account that is taken over can
# build a draft but cannot ship it. `build` is the same draft made here, for
# while CI cannot run; it has the Windows files only.
#
# `sign` downloads the draft, runs the Windows binaries, hashes every file,
# writes latest.json, signs it, checks the signature, and publishes once you
# say yes. To take a bad release back, make it a draft again:
#
#   gh release edit <tag> --draft=true
#
# and the previous one is the latest again. Copies that already updated keep
# it -- Letronna never moves back a version -- so the fix is a newer release.
#
# LETRONNA_RELEASE_KEY   the private key (default ~\.letronna-release\release.pem)
# LETRONNA_RELEASE_REPO  where releases are drafted (default gadlus/letronna)
# LETRONNA_RELEASE_BASE  where latest.json says the files are (default the
#                        account API, which hands them to signed-in apps only)

param(
    [Parameter(Mandatory = $true, Position = 0)]
    [ValidateSet('build', 'sign')]
    [string] $Verb,
    [Parameter(Position = 1)]
    [string] $Tag
)

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
$root = Split-Path $PSScriptRoot -Parent
$repo = if ($env:LETRONNA_RELEASE_REPO) { $env:LETRONNA_RELEASE_REPO } else { 'gadlus/letronna' }
$key = if ($env:LETRONNA_RELEASE_KEY) { $env:LETRONNA_RELEASE_KEY } else { Join-Path $HOME '.letronna-release\release.pem' }
# The musl builds are the terminal alone, listed so their SHA-256 is signed
# too; the updater never asks for them, since it goes by `linux-x64` and
# `linux-arm64`.
$targets = 'windows-x64', 'linux-x64', 'macos-arm64', 'linux-x64-musl', 'linux-arm64-musl'

function Fail([string] $message) {
    Write-Host "  $message" -ForegroundColor Red
    exit 1
}

function Done([string] $message) {
    Write-Host '  + ' -ForegroundColor Blue -NoNewline
    Write-Host $message -ForegroundColor DarkGray
}

# `gh` and `openssl` report failure only through their exit code.
function Checked([string] $what) {
    if ($LASTEXITCODE -ne 0) { Fail $what }
}

function Version {
    $line = Select-String -Path (Join-Path $root 'Cargo.toml') -Pattern '^version = "(.*)"' | Select-Object -First 1
    $line.Matches[0].Groups[1].Value
}

# What a binary answers to --version. Read from a pipe, which the window
# writes to even with no console of its own.
function Said([string] $program) {
    $info = New-Object Diagnostics.ProcessStartInfo $program, '--version'
    $info.RedirectStandardOutput = $true
    $info.UseShellExecute = $false
    $info.CreateNoWindow = $true
    $process = [Diagnostics.Process]::Start($info)
    if (-not $process.WaitForExit(20000)) {
        $process.Kill()
        Fail "$program did not answer --version."
    }
    ($process.StandardOutput.ReadToEnd().Trim() -split '\s+')[-1]
}

function Draft([string] $tag) {
    $json = gh release view $tag -R $repo --json isDraft 2>$null
    if ($LASTEXITCODE -ne 0) { return $null }
    ($json | ConvertFrom-Json).isDraft
}

function Build {
    $version = Version
    $tag = "v$version"
    if (git -C $root status --porcelain) {
        Fail 'Commit or stash your changes first: a release is built from a clean tree.'
    }
    if ((git -C $root tag --points-at HEAD) -notcontains $tag) {
        Fail "HEAD is not tagged $tag. Tag it and push the tag first: git tag $tag; git push origin $tag"
    }
    if ((Draft $tag) -eq $false) { Fail "$tag is already published." }
    if (-not $env:GPUI_FXC_PATH) {
        $fxc = Get-ChildItem 'C:\Program Files (x86)\Windows Kits\10\bin\*\x64\fxc.exe' |
            Sort-Object FullName -Descending | Select-Object -First 1
        if (-not $fxc) { Fail 'fxc.exe was not found: install the Windows SDK.' }
        $env:GPUI_FXC_PATH = $fxc.FullName
    }
    cargo build --release --locked --manifest-path (Join-Path $root 'Cargo.toml')
    Checked 'The build failed.'
    $release = Join-Path $root 'target\release'
    foreach ($bin in 'letronna', 'letronna-gui') {
        $said = Said (Join-Path $release "$bin.exe")
        if ($said -ne $version) { Fail "$bin.exe says $said, not $version." }
    }
    Done "built $version"

    $dist = Join-Path ([IO.Path]::GetTempPath()) "letronna-release-$tag"
    Remove-Item -Recurse -Force $dist -ErrorAction SilentlyContinue
    New-Item -ItemType Directory $dist | Out-Null
    # The same files, under the same names, as CI's Windows job.
    Compress-Archive -Path (Join-Path $release 'letronna.exe'), (Join-Path $release 'letronna-gui.exe'),
        (Join-Path $root 'LICENSE'), (Join-Path $root 'README.md') -DestinationPath (Join-Path $dist 'letronna-windows-x64.zip')
    Copy-Item (Join-Path $release 'letronna.exe') (Join-Path $dist 'letronna-windows-x64.exe')
    Copy-Item (Join-Path $release 'letronna-gui.exe') (Join-Path $dist 'letronna-gui-windows-x64.exe')
    $files = @(Get-ChildItem $dist | ForEach-Object { $_.FullName }) +
        (Join-Path $root 'scripts\install.ps1'), (Join-Path $root 'scripts\install.sh')

    if ($null -eq (Draft $tag)) {
        gh release create $tag -R $repo --draft --verify-tag --title "Letronna $version" `
            --notes "Built on Windows by scripts/release.ps1. Sign and publish with: scripts/release.ps1 sign $tag" @files
    } else {
        gh release upload $tag -R $repo --clobber @files
    }
    Checked "Could not upload the draft to $repo."
    Remove-Item -Recurse -Force $dist
    Done "drafted $tag on $repo; next: scripts/release.ps1 sign $tag"
}

# The keys the app trusts, read from where they are compiled in. The test key
# beside them is deliberately not among these.
function Trusted {
    $source = Get-Content -Raw (Join-Path $root 'crates\core\src\update.rs')
    $block = [regex]::Match($source, '(?s)const RELEASE_KEYS[^=]*=\s*&\[(.*?)\];')
    if (-not $block.Success) { Fail 'RELEASE_KEYS was not found in crates/core/src/update.rs.' }
    [regex]::Matches($block.Groups[1].Value, '[0-9a-f]{64}') | ForEach-Object { $_.Value }
}

function Openssl {
    # Git for Windows puts two on PATH; either will do.
    $found = Get-Command openssl -CommandType Application -ErrorAction SilentlyContinue | Select-Object -First 1
    if ($found) { return $found.Source }
    foreach ($path in 'C:\Program Files\Git\mingw64\bin\openssl.exe', 'C:\Program Files\Git\usr\bin\openssl.exe') {
        if (Test-Path $path) { return $path }
    }
    Fail 'openssl was not found. It comes with Git for Windows.'
}

# One file of the release as latest.json describes it, or nothing if the
# draft does not have it.
function Entry([string] $folder, [string] $name, [string] $base) {
    $path = Join-Path $folder $name
    if (-not (Test-Path $path)) { return $null }
    [ordered]@{
        url    = "$base/$name"
        sha256 = (Get-FileHash $path -Algorithm SHA256).Hash.ToLowerInvariant()
        size   = (Get-Item $path).Length
    }
}

function Sign([string] $tag) {
    if ($tag -notmatch '^v\d+\.\d+\.\d+(-[0-9A-Za-z.-]+)?$') {
        Fail 'Which release? For example: scripts/release.ps1 sign v0.1.0-alpha.2'
    }
    $version = $tag.Substring(1)
    $draft = Draft $tag
    if ($null -eq $draft) { Fail "There is no release $tag on $repo." }
    if (-not $draft) { Fail "$tag is already published." }
    if (-not (Test-Path $key)) { Fail "No key at $key. Set LETRONNA_RELEASE_KEY to where it is." }
    $openssl = Openssl
    $base = if ($env:LETRONNA_RELEASE_BASE) { $env:LETRONNA_RELEASE_BASE.TrimEnd('/') } else { "https://api.letronna.ai/app/releases/$tag" }

    $work = Join-Path ([IO.Path]::GetTempPath()) "letronna-sign-$tag"
    Remove-Item -Recurse -Force $work -ErrorAction SilentlyContinue
    New-Item -ItemType Directory $work | Out-Null
    gh release download $tag -R $repo -D $work -p 'letronna-*'
    Checked "Could not download $tag from $repo."
    Done "downloaded $tag"

    # CI ran each platform's binaries; the Windows ones run here as well,
    # since they are the ones this machine can run.
    foreach ($bin in 'letronna-windows-x64.exe', 'letronna-gui-windows-x64.exe') {
        $path = Join-Path $work $bin
        if (-not (Test-Path $path)) { Fail "The draft has no $bin." }
        $said = Said $path
        if ($said -ne $version) { Fail "$bin says $said, not $version." }
    }
    Done "the Windows binaries say $version"

    $platforms = [ordered]@{}
    foreach ($target in $targets) {
        $exe = if ($target -like 'windows-*') { '.exe' } else { '' }
        # On macOS the app is not replaced in place: the disk image is handed over.
        $window = if ($target -like 'macos-*') { Entry $work "letronna-$target.dmg" $base } else { Entry $work "letronna-gui-$target$exe" $base }
        $terminal = Entry $work "letronna-$target$exe" $base
        if ($window -or $terminal) {
            $platforms[$target] = [ordered]@{ window = $window; terminal = $terminal }
        }
    }
    $manifest = [ordered]@{
        version   = $version
        notes     = 'https://letronna.ai/changelog'
        platforms = $platforms
    }
    $latest = Join-Path $work 'latest.json'
    $signature = Join-Path $work 'latest.json.sig'
    # Signed byte for byte, so written without the BOM PowerShell would add.
    [IO.File]::WriteAllText($latest, ($manifest | ConvertTo-Json -Depth 5), (New-Object Text.UTF8Encoding $false))

    $secret = Read-Host "  Passphrase for $key" -AsSecureString
    $env:LETRONNA_RELEASE_PASS = [Runtime.InteropServices.Marshal]::PtrToStringAuto(
        [Runtime.InteropServices.Marshal]::SecureStringToBSTR($secret))
    try {
        $public = Join-Path $work 'public.pem'
        $der = Join-Path $work 'public.der'
        & $openssl pkey -in $key -passin env:LETRONNA_RELEASE_PASS -pubout -out $public
        Checked 'Could not read the key. Is the passphrase right?'
        & $openssl pkey -pubin -in $public -outform DER -out $der
        Checked 'Could not read the public half of the key.'
        $bytes = [IO.File]::ReadAllBytes($der)
        $hex = -join ($bytes[($bytes.Length - 32)..($bytes.Length - 1)] | ForEach-Object { $_.ToString('x2') })
        if ((Trusted) -notcontains $hex) {
            Fail "This key ($hex) is not one Letronna trusts: it is not in RELEASE_KEYS in crates/core/src/update.rs."
        }
        & $openssl pkeyutl -sign -inkey $key -passin env:LETRONNA_RELEASE_PASS -rawin -in $latest -out $signature
        Checked 'Signing failed.'
    } finally {
        Remove-Item Env:\LETRONNA_RELEASE_PASS -ErrorAction SilentlyContinue
    }
    & $openssl pkeyutl -verify -pubin -inkey $public -rawin -in $latest -sigfile $signature | Out-Null
    Checked 'The signature does not verify.'
    if ((Get-Item $signature).Length -ne 64) { Fail 'The signature is not an Ed25519 signature.' }
    Done "signed latest.json with $hex"

    Write-Host ''
    Get-Content $latest | ForEach-Object { Write-Host "    $_" -ForegroundColor DarkGray }
    Write-Host ''
    $answer = Read-Host "  Publish $tag to every signed-in Letronna? [y/N]"
    if ($answer -notmatch '^(y|yes)$') { Fail 'Not published. The draft is as it was.' }

    gh release upload $tag -R $repo --clobber $latest $signature
    Checked 'Could not upload latest.json.'
    gh release edit $tag -R $repo --draft=false --prerelease=false --latest
    Checked 'Could not publish the release.'
    Remove-Item -Recurse -Force $work
    Done "published $tag"
}

switch ($Verb) {
    'build' { Build }
    'sign' { Sign $Tag }
}
