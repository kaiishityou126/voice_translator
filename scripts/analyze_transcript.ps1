param([string]$Path)
$segs = Get-Content -LiteralPath $Path -Encoding UTF8 | Where-Object { $_.Trim() } | ForEach-Object { $_ | ConvertFrom-Json }

$kana = @($segs | Where-Object { $_.translated -match '[\u3040-\u30ff]' })

$rows = foreach ($s in $kana) {
  $t = [string]$s.translated
  $kanaN = ([regex]::Matches($t, '[\u3040-\u30ff]')).Count
  $hanN  = ([regex]::Matches($t, '[\u4e00-\u9fff]')).Count
  [pscustomobject]@{ id=$s.id; len=$t.Length; kana=$kanaN; han=$hanN; orig=$s.original; tr=$t }
}

$echo = @($rows | Where-Object { $_.kana -ge 6 -or ($_.kana / [math]::Max($_.len,1)) -gt 0.15 })
$partial = @($rows | Where-Object { -not ($_.kana -ge 6 -or ($_.kana / [math]::Max($_.len,1)) -gt 0.15) })

Write-Output ("A-full-echo: {0}" -f $echo.Count)
Write-Output ("B-partial-kana: {0}" -f $partial.Count)
Write-Output ""
Write-Output "===== A-worst-15 (by kana desc) ====="
$echo | Sort-Object kana -Descending | Select-Object -First 15 | ForEach-Object {
  Write-Output ("[#{0}] kana={1} han={2}" -f $_.id, $_.kana, $_.han)
  Write-Output ("   orig: {0}" -f $_.orig)
  Write-Output ("   tr  : {0}" -f $_.tr)
}
