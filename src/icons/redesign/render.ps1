$ErrorActionPreference = 'Stop'

Add-Type -AssemblyName System.Drawing

$script:Supersample = 4
$script:CanvasSize = 1024

function Get-EdgeExecutable {
    $candidates = @(
        (Join-Path ${env:ProgramFiles(x86)} 'Microsoft\Edge\Application\msedge.exe'),
        (Join-Path $env:ProgramFiles 'Microsoft\Edge\Application\msedge.exe'),
        (Join-Path $env:LOCALAPPDATA 'Microsoft\Edge\Application\msedge.exe')
    )

    foreach ($candidate in $candidates) {
        if ($candidate -and (Test-Path -LiteralPath $candidate -PathType Leaf)) {
            return $candidate
        }
    }

    throw 'Microsoft Edge is required to rasterize the canonical SVG.'
}

function Get-PngBytes {
    param(
        [System.Drawing.Bitmap]$Source,
        [int]$Size
    )

    $bitmap = [System.Drawing.Bitmap]::new(
        $Size,
        $Size,
        [System.Drawing.Imaging.PixelFormat]::Format32bppArgb
    )
    $graphics = [System.Drawing.Graphics]::FromImage($bitmap)
    $graphics.Clear([System.Drawing.Color]::Transparent)
    $graphics.CompositingMode = [System.Drawing.Drawing2D.CompositingMode]::SourceOver
    $graphics.CompositingQuality = [System.Drawing.Drawing2D.CompositingQuality]::HighQuality
    $graphics.InterpolationMode = [System.Drawing.Drawing2D.InterpolationMode]::HighQualityBicubic
    $graphics.PixelOffsetMode = [System.Drawing.Drawing2D.PixelOffsetMode]::HighQuality
    $graphics.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::HighQuality
    $graphics.DrawImage($Source, 0, 0, $Size, $Size)
    $graphics.Dispose()

    $stream = [System.IO.MemoryStream]::new()
    $bitmap.Save($stream, [System.Drawing.Imaging.ImageFormat]::Png)
    $bytes = $stream.ToArray()
    $stream.Dispose()
    $bitmap.Dispose()
    return ,$bytes
}

$sizes = @(16, 24, 32, 48, 64, 128, 256, 1024)
$renderSize = $script:CanvasSize * $script:Supersample
$svgPath = Join-Path $PSScriptRoot 'quota-usage-monitor.svg'
$renderPath = Join-Path $PSScriptRoot 'quota-usage-monitor-render.png'
$edgeProfile = Join-Path $PSScriptRoot ".edge-profile-$PID"
$svgUri = [System.Uri]::new((Resolve-Path -LiteralPath $svgPath).Path).AbsoluteUri
$edge = Get-EdgeExecutable
$sourceBitmap = $null

try {
    if (Test-Path -LiteralPath $renderPath) {
        Remove-Item -LiteralPath $renderPath -Force
    }
    if (Test-Path -LiteralPath $edgeProfile) {
        throw "Temporary Edge profile already exists: $edgeProfile"
    }
    $edgeArguments = @(
        '--headless=new',
        '--disable-gpu',
        '--no-first-run',
        '--no-default-browser-check',
        '--hide-scrollbars',
        '--default-background-color=00000000',
        "--user-data-dir=$edgeProfile",
        "--window-size=$($script:CanvasSize),$($script:CanvasSize)",
        "--force-device-scale-factor=$($script:Supersample)",
        "--screenshot=$renderPath",
        $svgUri
    )
    & $edge @edgeArguments
    $waitTimer = [System.Diagnostics.Stopwatch]::StartNew()
    while (-not (Test-Path -LiteralPath $renderPath -PathType Leaf) -and $waitTimer.Elapsed.TotalSeconds -lt 30) {
        Start-Sleep -Milliseconds 100
    }
    if (-not (Test-Path -LiteralPath $renderPath -PathType Leaf)) {
        throw 'Microsoft Edge failed to render the canonical SVG.'
    }

    $sourceBitmap = [System.Drawing.Bitmap]::FromFile($renderPath)
    if ($sourceBitmap.Width -ne $renderSize -or $sourceBitmap.Height -ne $renderSize) {
        throw "Unexpected SVG render size: $($sourceBitmap.Width)x$($sourceBitmap.Height); expected ${renderSize}x${renderSize}."
    }

    $pngs = @{}
    foreach ($size in $sizes) {
        $bytes = Get-PngBytes $sourceBitmap $size
        $pngs[$size] = $bytes
        [System.IO.File]::WriteAllBytes(
            (Join-Path $PSScriptRoot "quota-usage-monitor-$size.png"),
            $bytes
        )
    }
    $sourceBitmap.Dispose()
    $sourceBitmap = $null

    # PNG-compressed frames support alpha cleanly at every size on Windows 10/11.
    $icoSizes = @(16, 24, 32, 48, 64, 128, 256)
    $icoPath = Join-Path $PSScriptRoot 'quota-usage-monitor.ico'
    $stream = [System.IO.File]::Create($icoPath)
    $writer = [System.IO.BinaryWriter]::new($stream)
    $writer.Write([UInt16]0)
    $writer.Write([UInt16]1)
    $writer.Write([UInt16]$icoSizes.Count)
    $offset = 6 + (16 * $icoSizes.Count)
    foreach ($size in $icoSizes) {
        $bytes = $pngs[$size]
        $dimension = if ($size -eq 256) { [byte]0 } else { [byte]$size }
        $writer.Write($dimension)
        $writer.Write($dimension)
        $writer.Write([byte]0)
        $writer.Write([byte]0)
        $writer.Write([UInt16]1)
        $writer.Write([UInt16]32)
        $writer.Write([UInt32]$bytes.Length)
        $writer.Write([UInt32]$offset)
        $offset += $bytes.Length
    }
    foreach ($size in $icoSizes) {
        $writer.Write([byte[]]$pngs[$size])
    }
    $writer.Dispose()
    $stream.Dispose()

    Write-Output "Rendered PNG sizes from canonical SVG: $($sizes -join ', ')"
    Write-Output "Rendered multi-size ICO: $icoPath"
}
finally {
    if ($sourceBitmap) {
        $sourceBitmap.Dispose()
    }
    if (Test-Path -LiteralPath $renderPath) {
        Remove-Item -LiteralPath $renderPath -Force
    }
    for ($attempt = 0; $attempt -lt 20 -and (Test-Path -LiteralPath $edgeProfile); $attempt++) {
        Remove-Item -LiteralPath $edgeProfile -Recurse -Force -ErrorAction SilentlyContinue
        if (Test-Path -LiteralPath $edgeProfile) {
            Start-Sleep -Milliseconds 250
        }
    }
    if (Test-Path -LiteralPath $edgeProfile) {
        throw "Could not remove temporary Edge profile: $edgeProfile"
    }
}
