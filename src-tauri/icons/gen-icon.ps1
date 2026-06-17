# Generate app icon source: white rounded square + dark stroke + CJK char (translate)
Add-Type -AssemblyName System.Drawing

$size = 1024
$bmp = New-Object System.Drawing.Bitmap($size, $size)
$g = [System.Drawing.Graphics]::FromImage($bmp)
$g.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::AntiAlias
$g.TextRenderingHint = [System.Drawing.Text.TextRenderingHint]::AntiAliasGridFit
$g.Clear([System.Drawing.Color]::Transparent)

$radius = 180
$d = $radius * 2

$ink = [System.Drawing.Color]::FromArgb(255, 23, 22, 15)
$white = [System.Drawing.Color]::FromArgb(255, 255, 255, 255)

# white fill
$rect = New-Object System.Drawing.Rectangle(0, 0, ($size - 1), ($size - 1))
$path = New-Object System.Drawing.Drawing2D.GraphicsPath
$path.AddArc($rect.X, $rect.Y, $d, $d, 180, 90)
$path.AddArc(($rect.Right - $d), $rect.Y, $d, $d, 270, 90)
$path.AddArc(($rect.Right - $d), ($rect.Bottom - $d), $d, $d, 0, 90)
$path.AddArc($rect.X, ($rect.Bottom - $d), $d, $d, 90, 90)
$path.CloseFigure()
$fill = New-Object System.Drawing.SolidBrush($white)
$g.FillPath($fill, $path)

# dark stroke
$penW = 28
$pen = New-Object System.Drawing.Pen($ink, $penW)
$inset = [int]($penW / 2)
$rect2 = New-Object System.Drawing.Rectangle($inset, $inset, ($size - 1 - $penW), ($size - 1 - $penW))
$path2 = New-Object System.Drawing.Drawing2D.GraphicsPath
$path2.AddArc($rect2.X, $rect2.Y, $d, $d, 180, 90)
$path2.AddArc(($rect2.Right - $d), $rect2.Y, $d, $d, 270, 90)
$path2.AddArc(($rect2.Right - $d), ($rect2.Bottom - $d), $d, $d, 0, 90)
$path2.AddArc($rect2.X, ($rect2.Bottom - $d), $d, $d, 90, 90)
$path2.CloseFigure()
$g.DrawPath($pen, $path2)

# CJK char U+8BD1 via codepoint (avoid file-encoding issues)
$ch = [char]0x8BD1
$font = New-Object System.Drawing.Font("Microsoft YaHei", 540, [System.Drawing.FontStyle]::Bold, [System.Drawing.GraphicsUnit]::Pixel)
$textBrush = New-Object System.Drawing.SolidBrush($ink)
$sf = New-Object System.Drawing.StringFormat
$sf.Alignment = [System.Drawing.StringAlignment]::Center
$sf.LineAlignment = [System.Drawing.StringAlignment]::Center
$layout = New-Object System.Drawing.RectangleF(0, 30, $size, $size)
$g.DrawString($ch, $font, $textBrush, $layout, $sf)

$g.Dispose()
$out = Join-Path $PSScriptRoot "icon-source.png"
$bmp.Save($out, [System.Drawing.Imaging.ImageFormat]::Png)
$bmp.Dispose()
Write-Output ("saved: " + $out)
