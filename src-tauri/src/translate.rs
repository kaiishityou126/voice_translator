//! 多引擎翻译：OpenAI 兼容 / Google 免费(非官方 gtx)。
//! 引擎与 key 由前端配置。"none"(纯字幕)不走这里——pipeline 直接跳过翻译。

use std::io::{BufRead, BufReader};

use anyhow::{bail, Result};

use crate::config::RuntimeConfig;

/// `context` 为前若干段的 (原文, 译文)，按时间顺序，仅 LLM 引擎(openai/ollama)用作多轮上下文，
/// 提升代词/专名/句子连续性；Google 无对话上下文能力，直接忽略。
///
/// `on_chunk` 收到「截至目前的全量累计译文」：LLM 引擎流式下每增一段回调一次；
/// Google 无流式接口，仅在拿到终值后回调一次。返回值为最终完整译文。
pub fn translate(
    client: &reqwest::blocking::Client,
    cfg: &RuntimeConfig,
    text: &str,
    context: &[(String, String)],
    mut on_chunk: impl FnMut(&str),
) -> Result<String> {
    match cfg.translation_engine.as_str() {
        "google" => {
            let out = translate_google(client, cfg, text)?;
            on_chunk(&out);
            Ok(out)
        }
        // Ollama 本身就是 OpenAI 兼容接口，复用 chat_translate，固定本地地址、无需 key
        "ollama" => chat_translate(
            client,
            &cfg.ollama_base_url,
            "",
            &cfg.ollama_model,
            cfg.target_lang_name(),
            text,
            context,
            &mut on_chunk,
        ),
        _ => chat_translate(
            client,
            &cfg.llm_base_url,
            &cfg.llm_api_key,
            &cfg.llm_model,
            cfg.target_lang_name(),
            text,
            context,
            &mut on_chunk,
        ),
    }
}

// ---------------- OpenAI 兼容（OpenAI / Ollama 共用、流式 SSE） ----------------
/// 兜底重译：仅 LLM 路径（openai 兼容 / ollama）经过这里，Google / none 不走。
/// 先正常带上下文翻一次；若 whatlang 判定「译文与原文同一语种」（模型很可能没翻、原样吐回），
/// 清空多轮上下文重译一次——历史里混入的未翻译译文会诱导模型继续不翻，清空可切断这种传染。
fn chat_translate(
    client: &reqwest::blocking::Client,
    base_url: &str,
    api_key: &str,
    model: &str,
    target_name: &str,
    text: &str,
    context: &[(String, String)],
    on_chunk: &mut dyn FnMut(&str),
) -> Result<String> {
    // 第一次先憋住不推前端（sink 吞掉流式增量），等整句翻完再判定语种：
    // 这样若判定「没翻译/原样吐回」，这段未翻译内容既不显示也不会被上层写库，直接重译。
    let out = {
        let mut sink = |_: &str| {};
        chat_once(
            client, base_url, api_key, model, target_name, text, context, &mut sink,
        )?
    };
    if (same_language(text, &out) && !is_target_lang(text, target_name))
        || output_wrong_script(&out, target_name)
    {
        // 跟踪：打印触发原因 + 原文/首译,定位「日语漏成译文」等未翻译情形
        let reason = if output_wrong_script(&out, target_name) {
            "译文残留假名/谚文"
        } else {
            "whatlang 同语种"
        };
        eprintln!("[tr] 疑似未翻译({reason})，清空上下文重译  src=\"{text}\"  out1=\"{out}\"");
        // 清空多轮上下文重译，这次才把增量推给前端显示。
        return chat_once(
            client, base_url, api_key, model, target_name, text, &[], on_chunk,
        );
    }
    // 正常译文：整句一次性推前端（已无逐字流式，原文在 ASR 阶段已即时显示）。
    on_chunk(&out);
    Ok(out)
}

/// 译文里残留「源语言独有文字」时为强信号：模型没翻译。
/// - 目标非日语时,译文含平假名/片假名 → 一定没翻(中文/英文都不该有假名;汉字共用不算);
/// - 目标非韩语时,译文含谚文 → 没翻。
/// 比 whatlang 可靠：whatlang 区分不了中/日(日语汉字常被误判成中文)，假名/谚文是确定性脚本特征。
fn output_wrong_script(out: &str, target_name: &str) -> bool {
    let has_kana = out
        .chars()
        .any(|c| matches!(c as u32, 0x3040..=0x309F | 0x30A0..=0x30FF));
    let has_hangul = out
        .chars()
        .any(|c| matches!(c as u32, 0x1100..=0x11FF | 0xAC00..=0xD7A3));
    (has_kana && target_name != "Japanese") || (has_hangul && target_name != "Korean")
}

/// 用 whatlang 粗判两段文本是否同一语种，仅用于「译文是否还是原文那语种」的兜底判断：
/// 返回 true ≈ LLM 没翻译（把原文原样吐回）。文本过短时 whatlang 不可靠，直接返回 false（不重译）。
fn same_language(src: &str, out: &str) -> bool {
    if src.chars().count() < 4 || out.chars().count() < 4 {
        return false;
    }
    match (whatlang::detect_lang(src), whatlang::detect_lang(out)) {
        (Some(a), Some(b)) => a == b,
        _ => false,
    }
}

/// 源文本本身是否就是目标语（如目标中文、音频也是中文）：此时「译文与原文同语种」属正常，
/// 用户本就要同语种，不该重译。target_name 为提示词用的英文名，映射到 whatlang 语种。
fn is_target_lang(src: &str, target_name: &str) -> bool {
    let target = match target_name {
        "English" => whatlang::Lang::Eng,
        "Japanese" => whatlang::Lang::Jpn,
        "Korean" => whatlang::Lang::Kor,
        "French" => whatlang::Lang::Fra,
        "Spanish" => whatlang::Lang::Spa,
        "German" => whatlang::Lang::Deu,
        "Russian" => whatlang::Lang::Rus,
        _ => whatlang::Lang::Cmn, // Simplified Chinese / Cantonese
    };
    whatlang::detect_lang(src) == Some(target)
}

fn chat_once(
    client: &reqwest::blocking::Client,
    base_url: &str,
    api_key: &str,
    model: &str,
    target_name: &str,
    text: &str,
    context: &[(String, String)],
    on_chunk: &mut dyn FnMut(&str),
) -> Result<String> {
    let url = format!("{}/chat/completions", base_url.trim_end_matches('/'));
    let system = format!(
        "You are a professional real-time interpreter producing live subtitles. \
         Translate the user's text into {target}. \
         The input comes from live speech recognition and may contain recognition errors, missing or \
         wrong characters, run-on sentences, or no punctuation — infer the intended meaning from context \
         and produce one clean, natural, grammatical sentence in {target}. \
         Speech recognition frequently garbles PROPER NOUNS — especially personal names, politician \
         names, place names and organization names — into homophones or wrong characters. Using your \
         own world knowledge together with the surrounding context, silently identify and CORRECT \
         clearly misrecognized proper nouns to their correct real-world form BEFORE translating. \
         Only fix misrecognitions that the context makes obvious; never invent names that aren't there \
         and never replace a name you are unsure about. \
         The target language is fixed to {target} by the user. You MUST always output {target}, \
         no matter what language the source is in. Never copy the source text as-is and never \
         output any language other than {target}. Do NOT decide for yourself whether the text \
         is \"already translated\"; even if the whole input looks like it could be {target}, still \
         rewrite it as a clean, natural {target} sentence. Many Japanese kanji look identical to \
         Chinese characters but are NOT Chinese — always render them as natural {target}. Japanese \
         proper nouns, organization names, award titles and work titles MUST also be translated or \
         transliterated into {target}; never leave them in Japanese kana or Japanese wording. \
         Keep numbers, codes and Latin-letter proper nouns as they are, but render currency and units \
         in the natural {target} form (for example Japanese 円 means Japanese yen → 日元, never 元). \
         Preserve every number, digit, decimal, percentage and year EXACTLY as in the source — never \
         change their magnitude, scale or unit (for example 0.75% stays 0.75%, never 75%; a rate \
         reaching the 1% level must not become any other figure). \
         The content is third-person broadcast narration; unless the source explicitly uses first or \
         second person, do NOT introduce \"I\", \"we\" or \"you\" — keep it as neutral third-person narration. \
         Do not add, omit or embellish information; stay faithful to what was said. \
         Output ONLY the translation itself — no quotes, no notes, no explanations, no source text, \
         no pinyin or romanization. Keep it fluent, faithful and concise, fit for a single subtitle line.",
        target = target_name
    );
    // 多轮上下文：把前几段原文/译文作为 user/assistant 历史轮，给模型句子连续性参考
    let mut messages = vec![serde_json::json!({ "role": "system", "content": system })];
    for (orig, trans) in context {
        messages.push(serde_json::json!({ "role": "user", "content": orig }));
        messages.push(serde_json::json!({ "role": "assistant", "content": trans }));
    }
    // 前置强制：把「只输出目标语」的指令贴进本段 user 轮(而非只在 system)。
    // 多轮历史里一旦混进未翻译的原文(assistant 轮)，会诱导模型继续照抄；
    // 每段都重申一次，模型对最新 user 轮服从度高于 system，可盖过这种传染、强制按目标语翻。
    let forced = format!(
        "Translate the following into {target}. Output ONLY the {target} translation, \
         no source text, no notes:\n{text}",
        target = target_name,
        text = text,
    );
    messages.push(serde_json::json!({ "role": "user", "content": forced }));
    let mut body = serde_json::json!({
        "model": model,
        "temperature": 0.2,
        "stream": true,
        "messages": messages
    });
    glm_disable_thinking(&mut body, model);

    let mut req = client.post(url).json(&body);
    if !api_key.is_empty() {
        req = req.bearer_auth(api_key);
    }
    stream_chat(req, on_chunk)
}

/// GLM 系列默认开思维链，内容会进 reasoning_content 导致 content 为空；对 glm 模型显式关掉。
/// 其它厂商（如 Groq llama）不识别 thinking 字段会 400，故只在模型名含 glm 时注入。
fn glm_disable_thinking(body: &mut serde_json::Value, model: &str) {
    if model.to_ascii_lowercase().contains("glm") {
        if let Some(obj) = body.as_object_mut() {
            obj.insert("thinking".into(), serde_json::json!({ "type": "disabled" }));
        }
    }
}

/// 通用：发起一次流式 chat completion，逐行解析 SSE 累积 content，每增量回调全量累计。
/// 翻译与摘要共用这套 SSE 解析。
fn stream_chat(
    req: reqwest::blocking::RequestBuilder,
    on_chunk: &mut dyn FnMut(&str),
) -> Result<String> {
    let resp = req.send()?;
    let status = resp.status();
    if !status.is_success() {
        // 错误响应不是 SSE，按普通 JSON 读错误信息
        let v: serde_json::Value = resp.json().unwrap_or_else(|_| serde_json::json!({}));
        let msg = v
            .get("error")
            .and_then(|e| e.get("message"))
            .and_then(|m| m.as_str())
            .unwrap_or("");
        bail!("LLM {} {}", status, msg);
    }
    // 逐行读 SSE：每行 `data: {json}`，取 choices[0].delta.content 累加，遇 [DONE] 结束。
    let reader = BufReader::new(resp);
    let mut acc = String::new();
    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            // 流中途断：不报错，用已累计的 acc 收尾（避免已显示的半句被当失败清掉）
            Err(_) => break,
        };
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let data = match line.strip_prefix("data:") {
            Some(d) => d.trim(),
            None => continue, // 非 data 行（注释/event）跳过
        };
        if data == "[DONE]" {
            break;
        }
        // 容错：半行/坏 json 跳过，不中断整个流
        let v: serde_json::Value = match serde_json::from_str(data) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if let Some(delta) = v["choices"][0]["delta"]["content"].as_str() {
            if !delta.is_empty() {
                acc.push_str(delta);
                on_chunk(acc.trim());
            }
        } else if let Some(rc) = v["choices"][0]["delta"]["reasoning_content"].as_str() {
            // 兜底：模型若没关掉思考、把正文塞进 reasoning_content，也累计起来免得整段空
            if !rc.is_empty() {
                acc.push_str(rc);
                on_chunk(acc.trim());
            }
        }
    }
    let out = acc.trim().to_string();
    if out.is_empty() {
        bail!("LLM 返回空内容");
    }
    Ok(out)
}

/// 选 LLM 端点：(base_url, api_key, model)。ollama 无需 key。
fn llm_endpoint(cfg: &RuntimeConfig) -> (&str, &str, &str) {
    match cfg.translation_engine.as_str() {
        "ollama" => (cfg.ollama_base_url.as_str(), "", cfg.ollama_model.as_str()),
        _ => (
            cfg.llm_base_url.as_str(),
            cfg.llm_api_key.as_str(),
            cfg.llm_model.as_str(),
        ),
    }
}

/// 目标语言的本地名（用于强制 LLM 输出语言）。覆盖 UI 全部 9 种目标语。
fn target_lang_native(cfg: &RuntimeConfig) -> &'static str {
    match cfg.target_lang.as_str() {
        "en" => "English",
        "ja" => "日本語",
        "ko" => "한국어",
        "yue" => "粤语",
        "fr" => "Français",
        "es" => "Español",
        "de" => "Deutsch",
        "ru" => "Русский",
        _ => "简体中文",
    }
}

/// 把整段会话译文提炼成结构化重点（仅 LLM 引擎 openai/ollama）。
/// `on_chunk` 收到「截至目前的全量累计摘要」，流式回填前端。返回最终完整摘要。
///
/// 自适应分块：记录字数超过单次上下文预算时，切成多块先各自提炼要点（map），
/// 再把各块要点合并成最终纪要（reduce），避免一次性塞爆模型上下文导致截断/拒绝。
pub fn summarize(
    client: &reqwest::blocking::Client,
    cfg: &RuntimeConfig,
    transcript: &str,
    mut on_chunk: impl FnMut(&str),
    mut on_stage: impl FnMut(&str),
) -> Result<String> {
    // summary_max_context 为单次最大上下文（≈token，CJK 约等于字数）；
    // 留 ~2000 给 system 提示 + 模型输出，其余喂正文。
    let budget = cfg.summary_max_context.max(2000);
    let input_budget = budget.saturating_sub(2000).max(1500);
    let total = transcript.chars().count();

    // 单次喂给 LLM 的输入上限 = 用户设置（input_budget），不再写死硬顶。
    // 小模型一次吃数万字会退化打转、丢话题；旗舰模型能吃几万字。所以交给设置决定：
    // 调小防小模型退化，调大让旗舰一次吃完更连贯。map/collapse/reduce 全遵守。
    let safe_call = input_budget;

    // 装得下（≤ 安全上限）→ 一次性走结构化提炼（短会话无感，行为同旧版）
    if total <= safe_call {
        on_stage(&format!("单段直提：{} 字", total));
        let desc = "下面是一段实时转录的双语记录（[原] 原文 / [译] 译文，可能不完整、有口语冗余、识别有误）。";
        return summarize_structured(client, cfg, transcript, desc, &mut on_chunk);
    }

    // 装不下 → map-reduce
    // map chunk 故意远小于模型上下文：单段塞数万字会让模型退化打转、只覆盖两三个话题就耗尽输出，
    // 导致整段其余内容（含散落的新闻/天气/地震快讯）漏提。小而聚焦的段能让每段被完整覆盖。
    let map_chunk = safe_call;
    let chunks = chunk_by_chars(transcript, map_chunk);
    let m = chunks.len();
    on_stage(&format!("map 开始：{} 段，共 {} 字", m, total));
    let mut partials: Vec<String> = Vec::with_capacity(m);
    for (i, ch) in chunks.iter().enumerate() {
        on_stage(&format!("map {}/{} start", i + 1, m));
        // 进度抬头：map 阶段把实时要点显示在抬头下方，限流重试提示也走这里
        let header = format!("正在分段提炼…（{}/{} 段，共 {} 字）\n\n", i + 1, m, total);
        let mut cb = |acc: &str| on_chunk(&format!("{}{}", header, acc));
        let p = summarize_map_chunk(client, cfg, ch, &mut cb)?;
        on_stage(&format!("map {}/{} done（{} 字）", i + 1, m, p.chars().count()));
        partials.push(format!("# 片段 {}\n{}", i + 1, p.trim()));
    }
    // 分层折叠（collapse）：各片段要点拼起来若仍超预算，先分组递归折叠到装得下，
    // 再做最终 reduce——避免 reduce 一次性硬压把次要议题丢掉。
    let mut combined = partials.join("\n\n");
    let mut round = 0;
    const MAX_COLLAPSE_ROUNDS: usize = 3;
    while combined.chars().count() > safe_call {
        // 折叠分组同样用安全上限：一次喂数万字会让模型退化打转、
        // 把真实议题（地震/天气/资讯等）挤掉只剩口水，与 map 阶段是同一个坑。
        let groups = chunk_by_chars(&combined, safe_call);
        // 单组就超预算，折不动了；或折叠 3 轮仍不缩（模型不压缩）→ 停，交给 reduce 兜底，避免死循环
        if groups.len() <= 1 || round >= MAX_COLLAPSE_ROUNDS {
            break;
        }
        round += 1;
        let g = groups.len();
        on_stage(&format!("collapse 第 {} 轮：{} 组（{} 字）", round, g, combined.chars().count()));
        let mut folded: Vec<String> = Vec::with_capacity(g);
        for (i, grp) in groups.iter().enumerate() {
            let header = format!("正在归并要点…（第 {} 轮 {}/{} 组）\n\n", round, i + 1, g);
            let mut cb = |acc: &str| on_chunk(&format!("{}{}", header, acc));
            let p = summarize_map_chunk(client, cfg, grp, &mut cb)?;
            folded.push(p.trim().to_string());
        }
        combined = folded.join("\n\n");
    }
    on_stage(&format!("reduce：合并 {} 字生成最终纪要", combined.chars().count()));
    let desc = "下面是把一段内容分段提炼后的要点清单（按片段顺序排列，可能有重叠或重复）。\
                请按话题聚类、去重、归纳成最终纪要；有名有姓有数字的话题保留，零碎无名口水可省略。";
    summarize_structured(client, cfg, &combined, desc, &mut on_chunk)
}

/// 结构化提炼（reduce / 单次共用）：`input_desc` 说明输入是原始记录还是分段要点。
fn summarize_structured(
    client: &reqwest::blocking::Client,
    cfg: &RuntimeConfig,
    content: &str,
    input_desc: &str,
    on_chunk: &mut impl FnMut(&str),
) -> Result<String> {
    let (base_url, api_key, model) = llm_endpoint(cfg);
    let url = format!("{}/chat/completions", base_url.trim_end_matches('/'));
    // 小节标题与「无」字样随目标语言本地化，保证整篇摘要（含结构骨架）都用目标语言
    let (lang_native, h_overview, h_points, h_decisions, h_actions, h_followup, none_word) =
        match cfg.target_lang.as_str() {
            "en" => (
                "English",
                "Overview",
                "Key Points",
                "Decisions",
                "Action Items",
                "Follow-ups / Open Questions",
                "None",
            ),
            "ja" => (
                "日本語",
                "概要",
                "論点",
                "決定事項",
                "アクションアイテム",
                "フォローアップ / 未決事項",
                "なし",
            ),
            "ko" => (
                "한국어",
                "개요",
                "주요 논점",
                "결정 사항",
                "액션 아이템",
                "후속 조치 / 미결 사항",
                "없음",
            ),
            "fr" => (
                "Français",
                "Aperçu",
                "Points clés",
                "Décisions",
                "Actions à mener",
                "Suivi / Questions ouvertes",
                "Aucun",
            ),
            "es" => (
                "Español",
                "Resumen",
                "Puntos clave",
                "Decisiones",
                "Acciones",
                "Seguimiento / Pendientes",
                "Ninguno",
            ),
            "de" => (
                "Deutsch",
                "Überblick",
                "Kernpunkte",
                "Entscheidungen",
                "Aufgaben",
                "Offene Punkte",
                "Keine",
            ),
            "ru" => (
                "Русский",
                "Обзор",
                "Ключевые моменты",
                "Решения",
                "Задачи",
                "Незакрытые вопросы",
                "Нет",
            ),
            _ => (
                "简体中文",
                "概要",
                "讨论要点",
                "决定事项",
                "行动项",
                "待跟进 / 未决问题",
                "无",
            ),
        };
    let system = format!(
        "【输出语言强制要求】整篇回答必须 100% 用 {lang_native} 书写——包括所有小节标题、要点正文、关键词、说明文字，一个字都不许用其他语言（哪怕原始记录是别的语言）。\
         如果你用了 {lang_native} 以外的语言，就是错误输出。\n\n\
         你在分析一段录音/记录，先判断这是什么内容（视频会议、新闻、音乐、访谈、讲座、广播等）。{input_desc}\
         请用「{lang_native}」整理成分析型纪要：唱歌、歌词、语气词、寒暄、口水话只一句带过，但事件、事实、数字、金额、时间、人名都要保留。\
         归类铁律：把同一话题归到一条，按主题聚成 5-10 条要点，每条几句话归纳该话题、不要逐条搬运原句、不要把不同话题塞进同一条。\
         取舍：有名有姓、有数字的话题（如人名/案件/台风/赛事/汇率）必须保留；没人名没数字的零碎口水（如「某人决定加油」）可省略或一句带过。\
         切勿仅因两段共用某个词就把不同话题合并，不确定时宁可拆开。\
         每条要点是一个话题的小结，可多句：发生了什么、关键事实与数字，关键词用 Markdown 加粗，不同要点别重复同一件事。\
         严格按以下 Markdown 结构输出（标题与正文一律用 {lang_native}，层级不变）：\n\
         ## {h_overview}\n（2-3 句：这是什么内容、整体讲了什么，点到主要话题）\n\
         ## {h_points}\n- 每个话题一条「**关键词**：几句归纳」，按主题聚类，不流水账、不重复\n\
         ## {h_decisions}\n- 已拍板的结论；没有就写「{none_word}」\n\
         ## {h_actions}\n- 待办，尽量标负责人和时间；没有就写「{none_word}」\n\
         ## {h_followup}\n- 悬而未决、需确认的点；没有就写「{none_word}」\n\
         不要逐句翻译，不要编造记录中没有的信息。再次强调：最终输出从头到尾只能是 {lang_native}。",
    );
    let messages = vec![
        serde_json::json!({ "role": "system", "content": system }),
        serde_json::json!({ "role": "user", "content": content }),
    ];
    let mut body = serde_json::json!({
        "model": model,
        "temperature": 0.45,
        // 反重复：reduce 输入已是去重要点，重复风险低；temperature 略提到 0.45 兼顾稳定结构与表达，
        // penalty 对支持的模型生效、GLM 忽略无害，max_tokens 给较大上限避免最终纪要小节被截断
        "frequency_penalty": 0.3,
        "presence_penalty": 0.2,
        "max_tokens": 3000,
        "stream": true,
        "messages": messages
    });
    glm_disable_thinking(&mut body, model);
    let raw = stream_chat_retry(client, &url, api_key, &body, on_chunk)?;
    Ok(strip_degenerate_tail(&strip_think(&raw)))
}

/// map 阶段：把一个片段提炼成简洁要点 bullet 列表（不出小节标题），整段收齐后返回。
fn summarize_map_chunk(
    client: &reqwest::blocking::Client,
    cfg: &RuntimeConfig,
    chunk: &str,
    on_chunk: &mut dyn FnMut(&str),
) -> Result<String> {
    let (base_url, api_key, model) = llm_endpoint(cfg);
    let url = format!("{}/chat/completions", base_url.trim_end_matches('/'));
    let lang_native = target_lang_native(cfg);
    let system = format!(
        "下面是一段实时转录双语记录的一个片段（[原] 原文 / [译] 译文，可能不完整、有口语冗余、识别有误；也可能本身已是上一轮要点）。\
         用「{lang_native}」先把内容归到几个话题，每个话题一条要点：事件、事实、数字、人名、明确话题。\
         唱歌、歌词、语气词、寒暄、口水话只合并成一句带过（如「演唱了《X》等歌曲」），没信息的不列。\
         无名无实的零碎情节（如「某人决定加油」「某人向某人求救」这种没人名没数字的口水）不要单列，归不进话题就丢掉。\
         有名有姓、有数字（金额、票数、年份、时间、人数、幅度）的话题必留并原样保留数字；别为凑词把不同话题硬合并。\
         每条「- **话题**：一句概括」，抓主干、不逐句复述，雷同只留一条。\
         只输出 bullet，不要标题、不要开场白和总结套话。整段只能用 {lang_native}。",
    );
    let messages = vec![
        serde_json::json!({ "role": "system", "content": system }),
        serde_json::json!({ "role": "user", "content": chunk }),
    ];
    let mut body = serde_json::json!({
        "model": model,
        "temperature": 0.6,
        // 反重复：map 直接吃原始转录、重复短语多；temperature 提到 0.6 打破贪婪解码循环，
        // penalty 对支持的模型（OpenAI/Ollama）生效、GLM 等不支持的会忽略也无害，max_tokens 兜底防刷屏
        "frequency_penalty": 0.4,
        "presence_penalty": 0.3,
        "max_tokens": 1500,
        "stream": true,
        "messages": messages
    });
    glm_disable_thinking(&mut body, model);
    // 模型无关兼底：不依赖 penalty（GLM 不支持），落地前先切掉退化鬼打墙的尾巴、再折叠完全重复的 bullet，防退化刷屏污染下游
    let raw = stream_chat_retry(client, &url, api_key, &body, on_chunk)?;
    Ok(dedup_lines(&strip_degenerate_tail(&strip_think(&raw))))
}

/// 剥离推理模型输出的思维链：去掉 <think>…</think> 块（含未闭合的残留），
/// 并把模型多打的连续星号（如 ****关键词）归一成 **，避免 Markdown 渲染错乱。
fn strip_think(text: &str) -> String {
    let mut s = text.to_string();
    // 闭合的 think 块
    while let (Some(a), Some(b)) = (s.find("<think>"), s.find("</think>")) {
        if b > a {
            s.replace_range(a..b + "</think>".len(), "");
        } else {
            break;
        }
    }
    // 未闭合：只剩 </think> 收尾，丢前面所有思考
    if let Some(b) = s.find("</think>") {
        s = s[b + "</think>".len()..].to_string();
    }
    // 连续 3+ 星号收敛为 2 个
    let mut out = String::with_capacity(s.len());
    let mut stars = 0usize;
    for c in s.chars() {
        if c == '*' { stars += 1; continue; }
        if stars > 0 { out.push_str(if stars >= 2 { "**" } else { "*" }); stars = 0; }
        out.push(c);
    }
    if stars > 0 { out.push_str(if stars >= 2 { "**" } else { "*" }); }
    out.trim().to_string()
}

/// 把要点列表里完全重复的非空行折叠掉（保留首次出现），模型无关的反退化兜底。
/// LLM 退化时会把同一句 bullet 刷几十遍；GLM 之类不支持 penalty 的端点尤其需要这层。
fn dedup_lines(text: &str) -> String {
    let mut seen = std::collections::HashSet::new();
    let mut out: Vec<String> = Vec::new();
    for line in text.lines() {
        let key = line.trim();
        if key.is_empty() {
            // 退化时会刷一堆空行：连续空行只留一个
            if out.last().map_or(false, |l| l.trim().is_empty()) {
                continue;
            }
            out.push(line.to_string());
        } else if seen.insert(key.to_string()) {
            out.push(line.to_string());
        }
    }
    out.join("\n")
}

/// 模型无关的退化兜底：LLM 卡循环时会把一小段词反复刷几十上百遍（如「演出地点、演奏评价…」无限重复）。
/// 检测结尾是否为某个短单元（2-40 字符）连续重复 ≥6 次，是则砍到只留一份，把鬼打墙的尾巴切掉。
/// 不依赖任何模型的 penalty/停用词，纯文本侧防护，对所有引擎通用。
fn strip_degenerate_tail(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let n = chars.len();
    if n < 60 {
        return text.to_string();
    }
    for unit in 2..=40usize {
        if unit * 6 > n {
            break;
        }
        let mut reps = 1usize;
        let mut i = n;
        while i >= 2 * unit && chars[i - unit..i] == chars[i - 2 * unit..i - unit] {
            reps += 1;
            i -= unit;
        }
        if reps >= 6 {
            let keep = (i + unit).min(n);
            return chars[..keep].iter().collect();
        }
    }
    text.to_string()
}

/// 按字符预算把记录切块：以段落（"\n\n" 分隔）为最小单位，累加到接近预算就开新块，绝不切到段中间。
fn chunk_by_chars(text: &str, budget: usize) -> Vec<String> {
    let mut chunks = Vec::new();
    let mut cur = String::new();
    for seg in text.split("\n\n") {
        let seg_len = seg.chars().count();
        // 单段就超预算（异常超长字幕）：先冲刷当前块，再按字符硬切该段，避免单块顶破模型上下文
        if seg_len > budget {
            if !cur.is_empty() {
                chunks.push(std::mem::take(&mut cur));
            }
            let cs: Vec<char> = seg.chars().collect();
            for piece in cs.chunks(budget) {
                chunks.push(piece.iter().collect());
            }
            continue;
        }
        let cur_len = cur.chars().count();
        if !cur.is_empty() && cur_len + seg_len + 2 > budget {
            chunks.push(std::mem::take(&mut cur));
        }
        if !cur.is_empty() {
            cur.push_str("\n\n");
        }
        cur.push_str(seg);
    }
    if !cur.is_empty() {
        chunks.push(cur);
    }
    chunks
}

/// 错误是否可重试：服务端限流(429) / 过载(503/529) 等临时性问题，
/// 以及 reqwest 传输层的瞬时故障（连接/首字节超时、连接被重置/中断、读响应出错）。
/// 提炼走 map-reduce 会发几十次请求，任何一次网络抖动都不该让整轮前功尽弃。
fn is_retryable(e: &anyhow::Error) -> bool {
    let s = e.to_string();
    // LLM 业务侧临时错误
    s.contains("LLM 429")
        || s.contains("LLM 503")
        || s.contains("LLM 529")
        || s.contains("Too Many Requests")
        || s.contains("Service Unavailable")
        || s.contains("overloaded")
        || s.contains("访问量过大")
        // reqwest 传输层瞬时错误（顶层 message 文案）
        || s.contains("error sending request")
        || s.contains("error reading")
        || s.contains("timed out")
        || s.contains("connection reset")
        || s.contains("connection closed")
        || s.contains("connection aborted")
        || s.contains("connection error")
}

/// 带退避重试的流式请求：遇限流/过载，指数退避（2/4/8s）后重试，最多 4 次。
/// 重试间通过 `on_chunk` 回显等待提示。429 等错误在请求建连阶段返回，此时 acc 尚空，重试不会丢已显示内容。
fn stream_chat_retry(
    client: &reqwest::blocking::Client,
    url: &str,
    api_key: &str,
    body: &serde_json::Value,
    on_chunk: &mut dyn FnMut(&str),
) -> Result<String> {
    const MAX_ATTEMPTS: u32 = 4;
    let mut attempt = 0u32;
    loop {
        attempt += 1;
        let mut req = client.post(url).json(body);
        if !api_key.is_empty() {
            req = req.bearer_auth(api_key);
        }
        match stream_chat(req, on_chunk) {
            Ok(s) => return Ok(s),
            Err(e) => {
                if attempt < MAX_ATTEMPTS && is_retryable(&e) {
                    let wait = 1u64 << attempt; // 2 / 4 / 8 秒
                    on_chunk(&format!(
                        "⏳ 网络或接口波动（{}），{} 秒后自动重试（第 {}/{} 次）…",
                        e,
                        wait,
                        attempt,
                        MAX_ATTEMPTS - 1
                    ));
                    std::thread::sleep(std::time::Duration::from_secs(wait));
                    continue;
                }
                return Err(e);
            }
        }
    }
}

// ---------------- Google 免费（非官方 gtx 端点，无需 key） ----------------
fn translate_google(
    client: &reqwest::blocking::Client,
    cfg: &RuntimeConfig,
    text: &str,
) -> Result<String> {
    let sl = google_lang(&cfg.source_lang, true);
    let tl = google_lang(&cfg.target_lang, false);
    let resp = client
        .get("https://translate.googleapis.com/translate_a/single")
        .header("User-Agent", "Mozilla/5.0")
        .query(&[
            ("client", "gtx"),
            ("sl", sl),
            ("tl", tl),
            ("dt", "t"),
            ("q", text),
        ])
        .send()?;
    let status = resp.status();
    let v: serde_json::Value = resp.json()?;
    if !status.is_success() {
        bail!("Google 翻译 {}（非官方接口，可能被限流）", status);
    }
    // 响应是嵌套数组：v[0] 为分句数组，每个分句 seg[0] 是译文片段
    let mut out = String::new();
    if let Some(arr) = v.get(0).and_then(|x| x.as_array()) {
        for seg in arr {
            if let Some(s) = seg.get(0).and_then(|x| x.as_str()) {
                out.push_str(s);
            }
        }
    }
    let out = out.trim().to_string();
    if out.is_empty() {
        bail!("Google 翻译返回空");
    }
    Ok(out)
}

// ---------------- 语言代码映射 ----------------
fn google_lang(code: &str, allow_auto: bool) -> &'static str {
    match code {
        "zh" => "zh-CN",
        "en" => "en",
        "ja" => "ja",
        "ko" => "ko",
        "yue" => "yue", // 粤语
        "fr" => "fr",
        "es" => "es",
        "de" => "de",
        "ru" => "ru",
        _ => {
            if allow_auto {
                "auto"
            } else {
                "en"
            }
        }
    }
}
