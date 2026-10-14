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
        // "llm"（云端 OpenAI 兼容 / 本地 Ollama 统一）：按 translation_llm_id 从模型库取端点
        _ => {
            let (base_url, api_key, model) = translate_endpoint(cfg)?;
            chat_translate(
                client,
                base_url,
                api_key,
                model,
                cfg.target_lang_name(),
                text,
                context,
                &mut on_chunk,
            )
        }
    }
}

// ---------------- OpenAI 兼容（OpenAI / Ollama 共用、流式 SSE） ----------------
/// 兜底重译：仅 LLM 路径（openai 兼容 / ollama）经过这里，Google / none 不走。
/// 第一次就带上下文流式翻译并逐字推前端（低延迟）；翻完再事后补判「疑似没翻译」（whatlang 同语种，
/// 或译文残留源语言脚本），命中则清空多轮上下文重译一次，按同一 id 覆盖刚显示的内容——
/// 历史里混入的未翻译译文会诱导模型继续不翻，清空可切断这种传染。
/// 取舍：仅在罕见的 passthrough 坏情况下短暂闪现未翻文字，换取正常路径全程流式。
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
    // 第一次就把流式增量推前端（逐字显示、低延迟；原文已在 ASR 阶段即时显示）。
    let out = chat_once(
        client, base_url, api_key, model, target_name, text, context, on_chunk,
    )?;
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
        // 清空多轮上下文重译，同样流式推前端，按同一 id 覆盖掉上面已显示的疑似未翻内容。
        return chat_once(
            client, base_url, api_key, model, target_name, text, &[], on_chunk,
        );
    }
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
    Ok(stream_chat(req, on_chunk)?.text)
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

/// 通用：发起一次流式 chat completion，逐行解析 SSE，遇 [DONE] 结束。
/// 只显示/返回「正文」：正文里的 `<think>…</think>` 被剥掉;思考走独立的
/// `reasoning_content` 字段时不显示，仅当正文全空才用它兜底（某些模型把答案塞进了 reasoning）。
/// 翻译与摘要共用这套 SSE 解析。
/// 一次流式请求的结果。除正文外还带上「流是怎么结束的」，
/// 因为 SSE 被中途掐断时正文仍是半截却不报错，只看正文分辨不出来。
struct StreamOutcome {
    text: String,
    /// 收到过 `[DONE]` 或非空 finish_reason，即服务端明确告知已写完。
    finished_cleanly: bool,
    /// finish_reason == "length"：顶到 max_tokens 被砍，同参数重试必然同样被砍。
    length_capped: bool,
}

fn stream_chat(
    req: reqwest::blocking::RequestBuilder,
    on_chunk: &mut dyn FnMut(&str),
) -> Result<StreamOutcome> {
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
    // 逐行读 SSE：每行 `data: {json}`，累计 delta.content（正文）与 delta.reasoning_content（思考）。
    let reader = BufReader::new(resp);
    let mut content = String::new(); // 正文累计（可能内联 <think>）
    let mut reasoning = String::new(); // 思考链累计（不显示，正文全空时才兜底）
    let mut shown = String::new(); // 上次推给前端的显示串，避免重复推
    let mut finished_cleanly = false;
    let mut length_capped = false;
    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            // 流中途断：不报错，用已累计内容收尾（避免已显示的半句被当失败清掉）
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
            finished_cleanly = true;
            break;
        }
        // 容错：半行/坏 json 跳过，不中断整个流
        let v: serde_json::Value = match serde_json::from_str(data) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if let Some(fr) = v["choices"][0]["finish_reason"].as_str() {
            finished_cleanly = true;
            length_capped = fr == "length";
        }
        let delta = &v["choices"][0]["delta"];
        let mut grew = false;
        if let Some(c) = delta["content"].as_str() {
            if !c.is_empty() {
                content.push_str(c);
                grew = true;
            }
        }
        if let Some(rc) = delta["reasoning_content"].as_str() {
            if !rc.is_empty() {
                reasoning.push_str(rc);
            }
        }
        if grew {
            // 仅正文长了才刷新显示；<think> 未闭合时剥后为空，思考阶段自然不显示。
            let disp = if content.contains('<') {
                strip_think(&content)
            } else {
                content.clone()
            };
            let disp = disp.trim();
            if !disp.is_empty() && disp != shown {
                shown = disp.to_string();
                on_chunk(disp);
            }
        }
    }
    // 收尾：正文优先（剥掉 <think>）；正文空了才用 reasoning 兜底。
    let mut out = strip_think(&content).trim().to_string();
    if out.is_empty() {
        out = reasoning.trim().to_string();
    }
    if out.is_empty() {
        bail!("LLM 返回空内容");
    }
    Ok(StreamOutcome {
        text: out,
        finished_cleanly,
        length_capped,
    })
}

/// 按 id 在模型库找一条 → (base_url, api_key，本地 Ollama 无需 key 返回空串, model)。找不到 None。
fn profile_endpoint<'a>(cfg: &'a RuntimeConfig, id: &str) -> Option<(&'a str, &'a str, &'a str)> {
    cfg.llm_profiles.iter().find(|p| p.id == id).map(|p| {
        (
            p.base_url.as_str(),
            if p.is_local { "" } else { p.api_key.as_str() },
            p.model.as_str(),
        )
    })
}

/// 翻译端点：按 translation_llm_id 取模型库里的一条；未选/失效则报错。
fn translate_endpoint(cfg: &RuntimeConfig) -> Result<(&str, &str, &str)> {
    match profile_endpoint(cfg, &cfg.translation_llm_id) {
        Some(e) => Ok(e),
        None => bail!("未选择翻译模型，请在设置的「模型库管理」里添加并选一个用于翻译"),
    }
}

/// 提炼端点：优先 summary_llm_id；空/失效回退到翻译用模型；都没有则报错。
fn summary_endpoint(cfg: &RuntimeConfig) -> Result<(&str, &str, &str)> {
    if !cfg.summary_llm_id.is_empty() {
        if let Some(e) = profile_endpoint(cfg, &cfg.summary_llm_id) {
            return Ok(e);
        }
    }
    match profile_endpoint(cfg, &cfg.translation_llm_id) {
        Some(e) => Ok(e),
        None => bail!("未选择提炼模型，请在设置里选择提炼模型或先配置翻译模型"),
    }
}

/// 分段要点的属性标记词（map 打标、reduce 据此分派小节）。
/// 必须随目标语言本地化，否则会往目标语纪要里混入其他语言，违反输出语言铁律。
fn summary_tags(target_lang: &str) -> (&'static str, &'static str, &'static str, &'static str) {
    match target_lang {
        "en" => ("Decision", "Action", "Open", "Discussion"),
        "ja" => ("決定", "対応", "未決", "議論"),
        "ko" => ("결정", "실행", "미결", "논의"),
        "fr" => ("Décision", "Action", "Ouvert", "Discussion"),
        "es" => ("Decisión", "Acción", "Abierto", "Discusión"),
        "de" => ("Entscheidung", "Aufgabe", "Offen", "Diskussion"),
        "ru" => ("Решение", "Задача", "Открыто", "Обсуждение"),
        _ => ("决定", "待办", "未决", "讨论"),
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
        let desc = "下面是一段实时转录记录（[原] 原文 / [译] 译文，多为目标语译文；部分段翻译失败仅有源语原文、可能多语种混杂，识别也可能有误、有口语冗余）。请统一归纳成目标语纪要。";
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
        // 不用 `# ` 标题：reduce 的格式铁律只允许 `## `，输入里出现其他标题会带歪模型
        partials.push(format!("〔片段 {}〕\n{}", i + 1, p.trim()));
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
    // 分段要点带 [标记]，这里告诉 reduce 怎么用它分派小节，并明确禁止把标记写进最终纪要
    let (t_decision, t_action, t_open, t_talk) = summary_tags(&cfg.target_lang);
    let desc = format!(
        "下面是把一段内容分段提炼后的要点清单（按片段顺序排列，可能有重叠或重复）。\
         每条开头方括号里的 [{t_decision}] / [{t_action}] / [{t_open}] / [{t_talk}] 是分段时标注的属性，\
         请据此把该条内容分派到下面结构中对应的小节；标记只作线索，与内容明显不符时以内容为准。\
         **不要把方括号标记本身写进最终输出。**\
         〔片段 N〕只是内部分段标记，**输出里不许提及片段编号或分段处理过程**，读者只该看到一份完整纪要。\
         请按话题聚类、去重、归纳成最终纪要（取舍与写法一律按下面的规则）。"
    );
    summarize_structured(client, cfg, &combined, &desc, &mut on_chunk)
}

/// 结构化提炼（reduce / 单次共用）：`input_desc` 说明输入是原始记录还是分段要点。
fn summarize_structured(
    client: &reqwest::blocking::Client,
    cfg: &RuntimeConfig,
    content: &str,
    input_desc: &str,
    on_chunk: &mut impl FnMut(&str),
) -> Result<String> {
    let (base_url, api_key, model) = summary_endpoint(cfg)?;
    let url = format!("{}/chat/completions", base_url.trim_end_matches('/'));
    // 小节标题与「无」「未知」字样随目标语言本地化，保证整篇摘要（含结构骨架）都用目标语言
    let (
        lang_native,
        h_overview,
        h_points,
        h_decisions,
        h_actions,
        h_followup,
        none_word,
        unknown_word,
    ) =
        match cfg.target_lang.as_str() {
            "en" => (
                "English",
                "Overview",
                "Key Points",
                "Decisions",
                "Action Items",
                "Follow-ups / Open Questions",
                "None",
                "Unknown",
            ),
            "ja" => (
                "日本語",
                "概要",
                "論点",
                "決定事項",
                "アクションアイテム",
                "フォローアップ / 未決事項",
                "なし",
                "不明",
            ),
            "ko" => (
                "한국어",
                "개요",
                "주요 논점",
                "결정 사항",
                "액션 아이템",
                "후속 조치 / 미결 사항",
                "없음",
                "미정",
            ),
            "fr" => (
                "Français",
                "Aperçu",
                "Points clés",
                "Décisions",
                "Actions à mener",
                "Suivi / Questions ouvertes",
                "Aucun",
                "Inconnu",
            ),
            "es" => (
                "Español",
                "Resumen",
                "Puntos clave",
                "Decisiones",
                "Acciones",
                "Seguimiento / Pendientes",
                "Ninguno",
                "Desconocido",
            ),
            "de" => (
                "Deutsch",
                "Überblick",
                "Kernpunkte",
                "Entscheidungen",
                "Aufgaben",
                "Offene Punkte",
                "Keine",
                "Unbekannt",
            ),
            "ru" => (
                "Русский",
                "Обзор",
                "Ключевые моменты",
                "Решения",
                "Задачи",
                "Незакрытые вопросы",
                "Нет",
                "Неизвестно",
            ),
            _ => (
                "简体中文",
                "概要",
                "讨论要点",
                "决定事项",
                "行动项",
                "待跟进 / 未决问题",
                "无",
                "未知",
            ),
        };
    let system = format!(
        "【输出语言强制要求】整篇回答必须 100% 用 {lang_native} 书写——包括所有小节标题、要点正文、关键词、说明文字，一个字都不许用其他语言（哪怕原始记录是别的语言）。\
         如果你用了 {lang_native} 以外的语言，就是错误输出。\n\n\
         你在分析一段录音/记录，先判断这是什么内容（会议、访谈、讲座、新闻、节目等）。{input_desc}\
         请用「{lang_native}」整理成分析型纪要。\n\
         【最重要的一条：每条都要有干货】每条都必须让**没听过原内容的人直接读懂具体内容**，而不是只知道「讲过这个话题」。\
         写完每条自检一遍：读者看完这句，能知道结论、做法或数据是什么吗？不能就是不合格，回去把具体内容补上。\n\
         ❌「比较了 go 和 push 的区别」→ 读者仍不知道区别是什么\n\
         ✅「**go / push**：go 在目标页已在栈中时直接跳转、不新建；push 每次压栈、总是开新页」\n\
         ❌「讨论了各项目进度」→ 读者仍不知道进度如何\n\
         ✅「**订单确认模块**：主流程已完成，退款分支待联调」\n\
         「介绍了 / 讲解了 / 讨论了 / 提到了 / 说明了 / 分析了」这类只说「讲过」的写法一律不合格。\
         但**原内容本身就没讲清楚的，如实写「仅提及、未展开」，绝不许编造具体内容来凑**。\n\
         【取舍】看有没有结论或信息价值，不看有没有出现人名或数字；语气词、寒暄、无信息的闲聊一律略过或合并成一句带过。\
         同一话题归到一条，按主题聚类；**有几个话题就写几条**，不为凑数注水，也不为压缩条数丢掉真实话题；\
         切勿仅因两段共用某个词就把不同话题合并，不确定时宁可拆开。\
         关键词用 Markdown 加粗；数字（时间、金额、数量、名称）原样保留；「为什么」只在该结论有争议或反直觉时补一句。\n\
         【识别误差】语音识别常把专有名词（技术名词、人名、机构名）听成同音词。上下文能明确判断出正确写法的就按正确的写，\
         拿不准的原样保留，**绝不臆造**。\n\
         【单向内容】若这段是单向讲授或播报（课程、讲座、新闻、节目，没有多人协商拍板），\
         那「{h_decisions}」「{h_actions}」「{h_followup}」多半本来就没有，**如实写「{none_word}」，绝不为填满而编**，\
         把力气全放在「{h_points}」每条的具体内容上。\n\
         【格式铁律】整篇**只允许两种标记**：小节标题用 `## `，其余内容一律写成条目「- **关键词**：内容」。\
         **禁止三级及以下标题（###）、禁止表格、禁止代码块围栏、小节标题里不要加 emoji 或任何符号。**\
         每个条目必须是一整条「- **关键词**：内容」，内容可以写长，但**不许拆成标题加正文两行**。\n\
         严格按以下 Markdown 结构输出（标题与正文一律用 {lang_native}，层级与顺序都不许变）：\n\
         ## {h_overview}\n（这一节是 2-3 句连续文字，不用条目：这是什么内容、整体讲了什么、最重要的结论或信息是什么。**不要把下面各条的关键词罗列一遍**）\n\
         ## {h_decisions}\n- **关键词**：已拍板的结论。新闻/讲座里转述的「某机构决定了什么」属于内容本身，写进「{h_points}」而不是这里；没有就写「{none_word}」\n\
         ## {h_actions}\n- **负责人**：要做的事，一条一件。只写记录中**有人明确承诺要做、或被明确指派去做**的事；\
         **绝不要把内容本身改写成任务**——播报了暴雨预警不等于有人被指派去监测天气。\
         只有记录里明确把这件事交给某人时才写负责人（句中提到某人名字不算，如「张三上次说这方案不行」里张三不是负责人），\
         期限只写记录中明说的；明显该有人负责却没指明的，负责人位置写「{unknown_word}」；没有就写「{none_word}」\n\
         ## {h_points}\n- **关键词**：该话题的结论、做法与关键数据。按主题聚类，不同条目别重复同一件事\n\
         ## {h_followup}\n- **关键词**：悬而未决、需确认的点；没有就写「{none_word}」\n\
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
        // penalty 对支持的模型生效、GLM 忽略无害。要点条数不再设 5-10 上限，max_tokens 随之抬高防截断
        "frequency_penalty": 0.3,
        "presence_penalty": 0.2,
        "max_tokens": 4000,
        "stream": true,
        "messages": messages
    });
    glm_disable_thinking(&mut body, model);
    // 流被中断（网络抖动 / 服务端提前关连接）时 stream_chat 会把半截内容当成功返回，摘要少写几节却不报错。
    // 用 finished_cleanly 区分「真被掐断」与「模型本来就只写这么多」，只对前者重试——
    // 后者和顶到 max_tokens 的情况，同参数重试只会得到同样的结果，白等一轮。
    const EXPECTED_SECTIONS: usize = 5; // 与上面 prompt 里的小节数一致
    const MAX_TRIES: usize = 2;
    let mut best = String::new();
    let mut best_sections = 0usize;
    let mut best_is_last = true;
    for attempt in 1..=MAX_TRIES {
        let o = stream_chat_retry(client, &url, api_key, &body, on_chunk)?;
        let out = strip_degenerate_tail(&strip_think(&o.text));
        let n = md_section_count(&out);
        // 始终留住小节最全的一次，避免重试拿到更差的结果反而把好的覆盖掉
        if attempt == 1 || n > best_sections {
            best_sections = n;
            best = out;
            best_is_last = true;
        } else {
            best_is_last = false;
        }
        if n >= EXPECTED_SECTIONS || o.finished_cleanly || o.length_capped || attempt == MAX_TRIES {
            break;
        }
        on_chunk(&format!(
            "⏳ 摘要生成被中断（只写出 {} 个小节），正在重新生成…",
            n
        ));
    }
    // 采用的不是最后一次时，前端停在后一次（更差）的画面上，补推一次保持与存档一致
    if !best_is_last {
        on_chunk(&best);
    }
    Ok(best)
}

/// 统计 Markdown 顶层小节（`## `）数量；`###` 等更深层级不计入。
fn md_section_count(text: &str) -> usize {
    text.lines()
        .filter(|l| l.trim_start().starts_with("## "))
        .count()
}

/// map 阶段：把一个片段提炼成简洁要点 bullet 列表（不出小节标题），整段收齐后返回。
fn summarize_map_chunk(
    client: &reqwest::blocking::Client,
    cfg: &RuntimeConfig,
    chunk: &str,
    on_chunk: &mut dyn FnMut(&str),
) -> Result<String> {
    let (base_url, api_key, model) = summary_endpoint(cfg)?;
    let url = format!("{}/chat/completions", base_url.trim_end_matches('/'));
    let lang_native = target_lang_native(cfg);
    let (t_decision, t_action, t_open, t_talk) = summary_tags(&cfg.target_lang);
    let system = format!(
        "下面是一段实时转录记录的一个片段（[原] 原文 / [译] 译文，多为目标语译文；部分段翻译失败仅有源语原文、可能多语种混杂，识别也可能有误、有口语冗余；也可能本身已是上一轮要点）。\
         用「{lang_native}」把内容归到几个话题，每个话题一条要点。\
         每条格式：「- [标记] **话题**：归纳」。\
         标记从这四个中选一个：[{t_decision}]=本次记录中当场拍板的结论、[{t_action}]=有人明确承诺要做或被明确指派的事、\
         [{t_open}]=讨论了但没结论、[{t_talk}]=其余内容（讲授、播报、说明等一律用这个）；拿不准就用 [{t_talk}]。\
         **不要把内容本身改写成任务**——播报了暴雨预警不等于有人要去监测天气。\
         若输入的某条本身已带方括号标记，原样保留该标记。\
         归纳写 1-3 句，必须让**没听过原内容的人直接读懂具体内容**——后续汇总只能看到你这里写下的东西，\
         写成「介绍了X」「讨论了Y」等于把内容丢了。\n\
         ❌「比较了 go 和 push 的区别」  ✅「go 在目标页已在栈中时直接跳转不新建；push 每次压栈总是开新页」\n\
         结论、各方立场、理由、前提条件、分歧，以及具体事实与数字（时间、金额、数量、名称）都要写进去并原样保留数字。\
         **原内容本身就没讲清楚的，如实写「仅提及、未展开」，绝不许编造细节来凑。**\
         语音识别常把专有名词听成同音词，上下文能明确判断的按正确写法写，拿不准的原样保留、绝不臆造。\
         取舍看有没有结论或信息价值，不看有没有出现人名或数字；语气词、寒暄、无信息的闲聊合并成一句带过或直接丢掉。\
         别为凑词把不同话题硬合并，雷同只留一条。\
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
        // penalty 对支持的模型（OpenAI/Ollama）生效、GLM 等不支持的会忽略也无害。
        // 每条要点从「一句概括」放宽到 1-3 句带细节，max_tokens 随之抬高，防要点写到一半被截断
        "frequency_penalty": 0.4,
        "presence_penalty": 0.3,
        "max_tokens": 2500,
        "stream": true,
        "messages": messages
    });
    glm_disable_thinking(&mut body, model);
    // 这一段被掐断的话，丢掉的要点 reduce 无从补回、且全程不会报错，所以必须在这里拦住。
    // map 输出是平铺 bullet、没有小节结构可校验，只能靠 finished_cleanly 判断，并取更长的一份。
    const MAX_TRIES: usize = 2;
    let mut best = String::new();
    for attempt in 1..=MAX_TRIES {
        let o = stream_chat_retry(client, &url, api_key, &body, on_chunk)?;
        // 模型无关兼底：不依赖 penalty（GLM 不支持），落地前先切掉退化鬼打墙的尾巴、再折叠完全重复的 bullet，防退化刷屏污染下游
        let out = dedup_lines(&strip_degenerate_tail(&strip_think(&o.text)));
        if out.chars().count() > best.chars().count() {
            best = out;
        }
        if o.finished_cleanly || o.length_capped || attempt == MAX_TRIES {
            break;
        }
        on_chunk("⏳ 本段提炼被中断，正在重试…");
    }
    Ok(best)
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
    // 未闭合的 <think>（流式中思考尚未结束，还没等到 </think>）→ 丢弃它及其后内容
    if let Some(a) = s.find("<think>") {
        s.truncate(a);
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
        || s.contains("LLM 返回空内容")
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

/// 连接压根没建起来（网络断、代理挂了、DNS 不通）。这类错误每试一次都要先耗光 connect_timeout，
/// 而“等一等”并不会让断掉的线路自己好，故少试几轮、早点把真错误告诉用户。
fn is_connect_failure(e: &anyhow::Error) -> bool {
    let s = e.to_string();
    s.contains("error sending request")
        || s.contains("connection refused")
        || s.contains("dns error")
}

/// 带退避重试的流式请求：遇限流/过载，指数退避（2/4/8s）后重试，最多 4 次。
/// 重试间通过 `on_chunk` 回显等待提示。429 等错误在请求建连阶段返回，此时 acc 尚空，重试不会丢已显示内容。
fn stream_chat_retry(
    client: &reqwest::blocking::Client,
    url: &str,
    api_key: &str,
    body: &serde_json::Value,
    on_chunk: &mut dyn FnMut(&str),
) -> Result<StreamOutcome> {
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
                let max = if is_connect_failure(&e) { 2 } else { MAX_ATTEMPTS };
                if attempt < max && is_retryable(&e) {
                    let wait = 1u64 << attempt; // 2 / 4 / 8 秒
                    // {:#} 带出整条 error source 链：只打顶层的话永远只有一句「error sending request」，
                    // 看不出是 TLS 被重置、连接超时还是对端断开，没法自助排查。
                    on_chunk(&format!(
                        "⏳ 网络或接口波动（{:#}），{} 秒后自动重试（第 {}/{} 次，单次最长等 15 秒）…",
                        e,
                        wait,
                        attempt,
                        max - 1
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
        "hi" => "hi",
        "it" => "it",
        "pt" => "pt",
        _ => {
            if allow_auto {
                "auto"
            } else {
                "en"
            }
        }
    }
}
