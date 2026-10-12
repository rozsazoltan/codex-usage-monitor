$ErrorActionPreference = 'Stop'

Add-Type -AssemblyName System.Drawing

$script:CanvasSize = 1024.0
$script:Supersample = 4

function New-IconBitmap {
    param([int]$Size)

    $renderSize = $Size * $script:Supersample
    $renderBitmap = [System.Drawing.Bitmap]::new($renderSize, $renderSize, [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
    $graphics = [System.Drawing.Graphics]::FromImage($renderBitmap)
    $graphics.Clear([System.Drawing.Color]::Transparent)
    $graphics.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::AntiAlias
    $graphics.CompositingMode = [System.Drawing.Drawing2D.CompositingMode]::SourceOver
    $graphics.CompositingQuality = [System.Drawing.Drawing2D.CompositingQuality]::HighQuality
    $graphics.InterpolationMode = [System.Drawing.Drawing2D.InterpolationMode]::HighQualityBicubic
    $graphics.PixelOffsetMode = [System.Drawing.Drawing2D.PixelOffsetMode]::HighQuality
    $scale = $renderSize / $script:CanvasSize
    $transform = [System.Drawing.Drawing2D.Matrix]::new($scale, 0, 0, $scale, 0, 0)
    $graphics.Transform = $transform
    $transform.Dispose()

    $background = [System.Drawing.SolidBrush]::new([System.Drawing.ColorTranslator]::FromHtml('#272725'))
    $backgroundPath = [System.Drawing.Drawing2D.GraphicsPath]::new()
    $backgroundPath.AddArc(80, 80, 448, 448, 180, 90)
    $backgroundPath.AddArc(496, 80, 448, 448, 270, 90)
    $backgroundPath.AddArc(496, 496, 448, 448, 0, 90)
    $backgroundPath.AddArc(80, 496, 448, 448, 90, 90)
    $backgroundPath.CloseFigure()
    $graphics.FillPath($background, $backgroundPath)

    $usageBrush = [System.Drawing.SolidBrush]::new([System.Drawing.ColorTranslator]::FromHtml('#F1EFE8'))
    $usagePath = [System.Drawing.Drawing2D.GraphicsPath]::new()
    $usagePath.AddLine(366, 492, 658, 492)
    $usagePath.AddLine(658, 492, 658, 712)
    $usagePath.AddArc(586, 676, 72, 72, 0, 90)
    $usagePath.AddLine(622, 748, 402, 748)
    $usagePath.AddArc(366, 676, 72, 72, 90, 90)
    $usagePath.CloseFigure()
    $graphics.FillPath($usageBrush, $usagePath)

    $framePath = [System.Drawing.Drawing2D.GraphicsPath]::new()
    $framePath.AddArc(330, 240, 144, 144, 180, 90)
    $framePath.AddArc(550, 240, 144, 144, 270, 90)
    $framePath.AddArc(550, 640, 144, 144, 0, 90)
    $framePath.AddArc(330, 640, 144, 144, 90, 90)
    $framePath.CloseFigure()
    $framePen = [System.Drawing.Pen]::new([System.Drawing.ColorTranslator]::FromHtml('#F1EFE8'), 72)
    $graphics.DrawPath($framePen, $framePath)
    $tickPen = [System.Drawing.Pen]::new([System.Drawing.ColorTranslator]::FromHtml('#272725'), 28)
    $tickPen.StartCap = [System.Drawing.Drawing2D.LineCap]::Round
    $tickPen.EndCap = [System.Drawing.Drawing2D.LineCap]::Round
    foreach ($tickY in @(356, 420, 468)) {
        $graphics.DrawLine($tickPen, 328, $tickY, 366, $tickY)
    }

    $backgroundPath.Dispose()
    $background.Dispose()
    $usageBrush.Dispose()
    $usagePath.Dispose()
    $framePath.Dispose()
    $framePen.Dispose()
    $tickPen.Dispose()
    $graphics.Dispose()

    $bitmap = [System.Drawing.Bitmap]::new($Size, $Size, [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
    $downsample = [System.Drawing.Graphics]::FromImage($bitmap)
    $downsample.Clear([System.Drawing.Color]::Transparent)
    $downsample.InterpolationMode = [System.Drawing.Drawing2D.InterpolationMode]::HighQualityBicubic
    $downsample.CompositingQuality = [System.Drawing.Drawing2D.CompositingQuality]::HighQuality
    $downsample.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::HighQuality
    $downsample.DrawImage($renderBitmap, 0, 0, $Size, $Size)
    $downsample.Dispose()
    $renderBitmap.Dispose()
    return $bitmap
}

function Get-PngBytes {
    param([int]$Size)

    $bitmap = New-IconBitmap $Size
    $stream = [System.IO.MemoryStream]::new()
    $bitmap.Save($stream, [System.Drawing.Imaging.ImageFormat]::Png)
    $bytes = $stream.ToArray()
    $stream.Dispose()
    $bitmap.Dispose()
    return ,$bytes
}

$sizes = @(16, 24, 32, 48, 64, 128, 256, 1024)
$pngs = @{}
foreach ($size in $sizes) {
    $bytes = Get-PngBytes $size
    $pngs[$size] = $bytes
    [System.IO.File]::WriteAllBytes((Join-Path $PSScriptRoot "codex-usage-meter-$size.png"), $bytes)
}

# PNG-compressed frames support alpha cleanly at every size on Windows 10/11.
$icoSizes = @(16, 24, 32, 48, 64, 128, 256)
$icoPath = Join-Path $PSScriptRoot 'codex-usage-meter.ico'
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

Write-Output "Rendered PNG sizes: $($sizes -join ', ')"
Write-Output "Rendered multi-size ICO: $icoPath"
