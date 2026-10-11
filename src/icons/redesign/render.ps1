$ErrorActionPreference = 'Stop'

Add-Type -AssemblyName System.Drawing

$script:CanvasSize = 1024.0
$script:ArtworkScale = 0.76
$script:Palette = @(
    [System.Drawing.ColorTranslator]::FromHtml('#31D6C3'),
    [System.Drawing.ColorTranslator]::FromHtml('#438FFF'),
    [System.Drawing.ColorTranslator]::FromHtml('#8D76FF')
)

function New-MeterBrush {
    $brush = [System.Drawing.Drawing2D.LinearGradientBrush]::new(
        [System.Drawing.PointF]::new(120, 120),
        [System.Drawing.PointF]::new(904, 904),
        $script:Palette[0],
        $script:Palette[2]
    )
    $blend = [System.Drawing.Drawing2D.ColorBlend]::new(3)
    $blend.Colors = $script:Palette
    $blend.Positions = [single[]]@(0, 0.52, 1)
    $brush.InterpolationColors = $blend
    return $brush
}

function Add-RoundedBar {
    param(
        [System.Drawing.Graphics]$Graphics,
        [System.Drawing.Brush]$Brush,
        [single]$X,
        [single]$Y,
        [single]$Width,
        [single]$Height
    )

    $radius = $Width / 2
    $path = [System.Drawing.Drawing2D.GraphicsPath]::new()
    $path.AddArc($X, $Y, $Width, $Width, 180, 90)
    $path.AddArc($X, $Y, $Width, $Width, 270, 90)
    $path.AddLine($X + $Width, $Y + $radius, $X + $Width, $Y + $Height - $radius)
    $path.AddArc($X, $Y + $Height - $Width, $Width, $Width, 0, 90)
    $path.AddArc($X, $Y + $Height - $Width, $Width, $Width, 90, 90)
    $path.CloseFigure()
    $Graphics.FillPath($Brush, $path)
    $path.Dispose()
}

function New-IconBitmap {
    param([int]$Size)

    $bitmap = [System.Drawing.Bitmap]::new($Size, $Size, [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
    $graphics = [System.Drawing.Graphics]::FromImage($bitmap)
    $graphics.Clear([System.Drawing.Color]::Transparent)
    $graphics.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::AntiAlias
    $graphics.CompositingMode = [System.Drawing.Drawing2D.CompositingMode]::SourceOver
    $graphics.CompositingQuality = [System.Drawing.Drawing2D.CompositingQuality]::HighQuality
    $graphics.InterpolationMode = [System.Drawing.Drawing2D.InterpolationMode]::HighQualityBicubic
    $graphics.PixelOffsetMode = [System.Drawing.Drawing2D.PixelOffsetMode]::HighQuality
    $scale = $Size / $script:CanvasSize * $script:ArtworkScale
    $offset = ($Size - ($script:CanvasSize * $scale)) / 2
    $transform = [System.Drawing.Drawing2D.Matrix]::new($scale, 0, 0, $scale, $offset, $offset)
    $graphics.Transform = $transform
    $transform.Dispose()

    $tickPen = [System.Drawing.Pen]::new([System.Drawing.ColorTranslator]::FromHtml('#C7DFFF'), 15)
    $tickPen.StartCap = [System.Drawing.Drawing2D.LineCap]::Round
    $tickPen.EndCap = [System.Drawing.Drawing2D.LineCap]::Round
    foreach ($tick in @(
        @{ X1 = 341; Y1 = 66; X2 = 329; Y2 = 35 },
        @{ X1 = 512; Y1 = 64; X2 = 512; Y2 = 28 },
        @{ X1 = 683; Y1 = 66; X2 = 695; Y2 = 35 }
    )) {
        $graphics.DrawLine($tickPen, $tick.X1, $tick.Y1, $tick.X2, $tick.Y2)
    }
    $tickPen.Dispose()

    $meterBrush = New-MeterBrush
    $arcPen = [System.Drawing.Pen]::new($meterBrush, 96)
    $arcPen.StartCap = [System.Drawing.Drawing2D.LineCap]::Round
    $arcPen.EndCap = [System.Drawing.Drawing2D.LineCap]::Round
    $graphics.DrawArc($arcPen, 88, 88, 848, 848, -58, 296)

    Add-RoundedBar $graphics $meterBrush 304 500 96 224
    Add-RoundedBar $graphics $meterBrush 464 402 96 322
    Add-RoundedBar $graphics $meterBrush 624 304 96 420

    $dotFill = [System.Drawing.SolidBrush]::new([System.Drawing.ColorTranslator]::FromHtml('#FFC65A'))
    $dotOutline = [System.Drawing.Pen]::new([System.Drawing.Color]::White, 10)
    $graphics.FillEllipse($dotFill, 703, 118, 68, 68)
    $graphics.DrawEllipse($dotOutline, 703, 118, 68, 68)

    $dotOutline.Dispose()
    $dotFill.Dispose()
    $arcPen.Dispose()
    $meterBrush.Dispose()
    $graphics.Dispose()
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
