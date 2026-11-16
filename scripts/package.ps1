param([switch]$SkipBuild, [string]$Executable, [string]$NsisCompiler, [string]$OutputDirectory)
$ErrorActionPreference = 'Stop'
$projectRoot = Split-Path $PSScriptRoot -Parent
Push-Location $projectRoot
try {
    $version = [regex]::Match((Get-Content Cargo.toml -Raw), '(?m)^version = "([^"]+)"').Groups[1].Value
    # 发布版不嵌入构建机器的用户目录和项目路径：依赖源码路径换成中性前缀，PDB 只记文件名。
    # 用 CARGO_ENCODED_RUSTFLAGS（\x1f 分隔）避免路径里的空格被拆开；它会覆盖 .cargo/config.toml 的 rustflags，所以带上 crt-static。
    $cargoHome = if ($env:CARGO_HOME) { $env:CARGO_HOME } else { Join-Path $env:USERPROFILE '.cargo' }
    $env:CARGO_ENCODED_RUSTFLAGS = @('-C', 'target-feature=+crt-static', "--remap-path-prefix=$cargoHome=cargo", "--remap-path-prefix=$projectRoot=daozai", '-C', 'link-arg=/PDBALTPATH:%_PDB%') -join [char]0x1f
    if (-not $SkipBuild) {
        $env:CARGO_BUILD_JOBS = '1'
        cargo +stable-x86_64-pc-windows-msvc build --locked --release --bin IslandHop --target x86_64-pc-windows-msvc
        if ($LASTEXITCODE -ne 0) { throw 'Release build failed' }
    }
    if (-not $Executable) {
        $targetDirectory = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { Join-Path $projectRoot 'target' }
        $Executable = Join-Path $targetDirectory 'x86_64-pc-windows-msvc\release\IslandHop.exe'
    }
    $binary = Get-Item -LiteralPath $Executable
    & (Join-Path $PSScriptRoot 'check-runtime.ps1') -Executable $binary.FullName
    if ($binary.VersionInfo.FileVersion -ne $version -and $binary.VersionInfo.FileVersion -ne "$version.0") { throw 'Executable version mismatch' }
    $content = [Text.Encoding]::UTF8.GetString([IO.File]::ReadAllBytes($binary.FullName))
    foreach ($private in @($env:USERPROFILE, $projectRoot)) {
        if ($private -and $content.Contains($private)) { throw "Executable embeds a build-machine path: $private" }
    }
    if (-not $NsisCompiler) {
        $compiler = Get-Command makensis.exe -ErrorAction SilentlyContinue
        if ($compiler) { $NsisCompiler = $compiler.Source }
        elseif ($env:NSIS_HOME) { $NsisCompiler = Join-Path $env:NSIS_HOME 'makensis.exe' }
        else { $NsisCompiler = Join-Path ${env:ProgramFiles(x86)} 'NSIS\makensis.exe' }
    }
    if (-not (Test-Path -LiteralPath $NsisCompiler)) { throw 'NSIS compiler not found; install NSIS or pass -NsisCompiler' }
    $packageName = "IslandHop-v$version-windows-x64"
    if (-not $OutputDirectory) { $OutputDirectory = Join-Path $projectRoot 'dist' }
    $outputDirectory = [IO.Path]::GetFullPath($OutputDirectory)
    $stagingDirectory = Join-Path $outputDirectory $packageName
    if (Test-Path -LiteralPath $stagingDirectory) { throw 'Package directory already exists' }
    New-Item -ItemType Directory -Path $stagingDirectory -Force | Out-Null
    # 压缩包内仍叫 daozai.exe：NSIS 安装器、update.ps1 与应用内更新器 (updates.rs) 都按这个名字找文件。
    Copy-Item -LiteralPath $binary.FullName -Destination (Join-Path $stagingDirectory 'daozai.exe')
    # ZIP 保留给免安装使用和应用内更新；普通用户只需下载安装 EXE。
    @('LICENSE','THIRD_PARTY_NOTICES.txt') | ForEach-Object {
        Copy-Item -LiteralPath (Join-Path $projectRoot $_) -Destination $stagingDirectory -Recurse
    }
    # 品牌统一为 IslandHop（页面按文件名解析版本号，混用 Daozai/IslandHop 会显得混乱）。
    # 注册表键 HKCU\...\Uninstall\Daozai 与压缩包内的 daozai.exe 是历史承载名，暂不动。
    $setup = Join-Path $outputDirectory "IslandHop-Setup-v$version-x64.exe"
    & $NsisCompiler /V2 /WX "/DAPP_VERSION=$version" "/DPROJECT_DIR=$projectRoot" "/DSOURCE_EXE=$($binary.FullName)" "/DOUTPUT_FILE=$setup" (Join-Path $projectRoot 'installer\setup.nsi')
    if ($LASTEXITCODE -ne 0) { throw 'Installer build failed' }
    $archive = Join-Path $outputDirectory "$packageName.zip"
    Compress-Archive -LiteralPath $stagingDirectory -DestinationPath $archive
    @($setup, $archive) | ForEach-Object {
        $checksum = (Get-FileHash -LiteralPath $_ -Algorithm SHA256).Hash.ToLowerInvariant()
        "$checksum  $([IO.Path]::GetFileName($_))"
    } | Set-Content -LiteralPath (Join-Path $outputDirectory 'SHA256SUMS.txt') -Encoding ascii

    # 应用内更新清单（客户端优先读自有 feed 的 manifest-windows.json）。
    # url 指向 zip：update.ps1 按包内 daozai.exe 原地覆盖；Setup exe 只作用户
    # 手动下载的安装入口（固定目录 + 快捷方式），不参与 OTA 解包。
    # 服务器端 sync-islandhop-update.sh 会把 url 重写成门户地址并镜像包体。
    $zipName = [IO.Path]::GetFileName($archive)
    $zipHash = (Get-FileHash -LiteralPath $archive -Algorithm SHA256).Hash.ToLowerInvariant()
    $zipSize = (Get-Item -LiteralPath $archive).Length
    $feedUrl = "https://github.com/thuki20/IslandHop/releases/download/v$version/$zipName"
    $manifest = [ordered]@{
        version = $version
        notes   = "IslandHop v$version`n- 覆盖安装请下载 IslandHop-Setup-v$version-x64.exe：固定安装目录、自动创建桌面/开始菜单快捷方式，升级保留数据。`n- 免安装/应用内更新使用 $zipName。"
        url     = $feedUrl
        sha256  = $zipHash
        size    = $zipSize
    }
    $manifestPath = Join-Path $outputDirectory 'manifest-windows.json'
    ($manifest | ConvertTo-Json -Depth 4) | Set-Content -LiteralPath $manifestPath -Encoding utf8

    Write-Output $setup
    Write-Output $archive
    Write-Output $manifestPath
} finally { Pop-Location }
