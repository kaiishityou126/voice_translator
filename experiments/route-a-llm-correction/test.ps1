# 路线甲验证：用当前翻译 LLM(GLM-4-flash) + "纠正专有名词" prompt，
# 看能否把 ASR 的同音误字(高市奈総)靠世界知识修成正确人名(高市早苗)，再翻成中文。
# 纯实验，不碰主工程。key 从 settings.json 读取，绝不打印。
$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = [System.Text.Encoding]::UTF8

$p = "$env:APPDATA\com.administrator.voicetranslator\settings.json"
$s = Get-Content $p -Raw | ConvertFrom-Json
$base = $s.llmBaseUrl.TrimEnd('/')
$key = $s.llmApiKey
$model = $s.llmModel
$url = "$base/chat/completions"
$target = 'Chinese'

# ① 基线 = 主工程当前的 system prompt(target=Chinese)
$baseSys = @"
You are a professional real-time interpreter producing live subtitles. Translate the user's text into $target. The input comes from live speech recognition and may contain recognition errors, missing or wrong characters, run-on sentences, or no punctuation — infer the intended meaning from context and produce one clean, natural, grammatical sentence in $target. The target language is fixed to $target by the user. You MUST always output $target, no matter what language the source is in. Never copy the source text as-is and never output any language other than $target. Many Japanese kanji look identical to Chinese characters but are NOT Chinese — always render them as natural $target. Japanese proper nouns, organization names, award titles and work titles MUST also be translated or transliterated into $target. Keep numbers, codes and Latin-letter proper nouns as they are. The content is third-person broadcast narration. Output ONLY the translation itself — no quotes, no notes, no explanations.
"@

# ② 纠错增强 = 基线 + 一句"用世界知识纠正被识别错的专有名词/人名"(不写死任何具体名字)
$correctClause = " IMPORTANT: The audio is Japanese broadcast/news speech. Speech recognition frequently garbles PROPER NOUNS — especially personal names, politician names, place names and organization names — into homophones or wrong kanji. Using your own world knowledge together with the surrounding context, silently identify and CORRECT clearly misrecognized proper nouns to their correct real-world form BEFORE translating. Only fix obvious misrecognitions implied by context; never invent names that aren't there."
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

# 测试素材 = NHK 国会辩论真实 ASR 输出(含同音误字)
$tests = @(
    '内閣総理大臣高市奈総さん。',
    '会議の中で具体的な議論をしていただくように、おのてら議長に高橋総理からお伝えいただくことできないでしょう。',
    '会議そして実も社会議の枠組みがあるわけですから、あ消費者、あ消費税減税。'
)

"模型: $model  目标语: $target`n"
foreach ($t in $tests) {
    "===== 输入(ASR原始): $t"
    "  [基线 prompt ] " + (Ask $baseSys $t)
    "  [纠错 prompt ] " + (Ask $correctSys $t)
    ""
}
