# Route A verification (ASCII-only source to survive PS5.1 GBK code page).
# Japanese test sentences are read from tests.txt (UTF-8). Key read from settings.json, never printed.
$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = [System.Text.Encoding]::UTF8

$p = "$env:APPDATA\com.administrator.voicetranslator\settings.json"
$s = Get-Content $p -Raw -Encoding UTF8 | ConvertFrom-Json
$base = $s.llmBaseUrl.TrimEnd('/')
$key = $s.llmApiKey
$model = $s.llmModel
$url = "$base/chat/completions"
$target = 'Chinese'

# (1) baseline = main app's current system prompt (target=Chinese)
$baseSys = "You are a professional real-time interpreter producing live subtitles. Translate the user's text into $target. The input comes from live speech recognition and may contain recognition errors, missing or wrong characters, run-on sentences, or no punctuation - infer the intended meaning from context and produce one clean, natural, grammatical sentence in $target. The target language is fixed to $target by the user. You MUST always output $target, no matter what language the source is in. Never copy the source text as-is and never output any language other than $target. Many Japanese kanji look identical to Chinese characters but are NOT Chinese - always render them as natural $target. Japanese proper nouns, organization names, award titles and work titles MUST also be translated or transliterated into $target. Keep numbers, codes and Latin-letter proper nouns as they are. The content is third-person broadcast narration. Output ONLY the translation itself - no quotes, no notes, no explanations."

# (2) correction-enhanced = baseline + a generic clause to fix misrecognized proper nouns
#     (deliberately does NOT name any specific person, to test the model's own world knowledge)
$correctClause = " IMPORTANT: The audio is Japanese broadcast/news speech. Speech recognition frequently garbles PROPER NOUNS - especially personal names, politician names, place names and organization names - into homophones or wrong kanji. Using your own world knowledge together with the surrounding context, silently identify and CORRECT clearly misrecognized proper nouns to their correct real-world form BEFORE translating. Only fix obvious misrecognitions implied by context; never invent names that aren't there."
$correctSys = $baseSys + $correctClause

function Ask($sys, $text) {
    $body = @{
        model       = $model
        temperature = 0.2
        stream      = $false
        messages    = @(
            @{ role = 'system'; content = $sys },
            @{ role = 'user'; content = $text }
        )
    } | ConvertTo-Json -Depth 6
    $headers = @{ Authorization = "Bearer $key" }
    $bytes = [System.Text.Encoding]::UTF8.GetBytes($body)
    $r = Invoke-RestMethod -Uri $url -Method Post -Headers $headers -ContentType 'application/json; charset=utf-8' -Body $bytes
    return $r.choices[0].message.content.Trim()
}

# 必须用 .NET ReadAllLines 显式 UTF8：PS5.1 的 Get-Content 对单行文件返回字符串导致 [0] 取到单字，且默认按 GBK 码页。
$tests = [IO.File]::ReadAllLines("$PSScriptRoot\tests.txt", [Text.Encoding]::UTF8) | Where-Object { $_.Trim() -ne '' }

"model: $model  target: $target`n"
foreach ($t in $tests) {
    "===== ASR input: $t"
    "  [baseline ] " + (Ask $baseSys $t)
    "  [corrected] " + (Ask $correctSys $t)
    ""
}
