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
    let url = format!("{}/chat/completions", base_url.trim_end_matches('/'));
    let system = format!(
        "You are a professional real-time interpreter producing live subtitles. \
         Translate the user's text into {target}. \
         The input comes from live speech recognition and may contain recognition errors, missing or \
         wrong characters, run-on sentences, or no punctuation — infer the intended meaning from context \
         and produce one clean, natural, grammatical sentence in {target}. \
         The source text may be in any language; always translate the whole text into {target}, \
         regardless of the source language. Many Japanese kanji look identical to Chinese characters \
         but are NOT Chinese — translate them into natural {target}. Only return the text unchanged \
         if it is genuinely and entirely already in {target}. \
         Keep numbers, codes and Latin-letter proper nouns as they are, but render currency and units \
         in the natural {target} form (for example Japanese 円 means Japanese yen → 日元, never 元). \
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
    messages.push(serde_json::json!({ "role": "user", "content": text }));
    let body = serde_json::json!({
        "model": model,
        "temperature": 0.2,
        "stream": true,
        "messages": messages
    });

    let mut req = client.post(url).json(&body);
    if !api_key.is_empty() {
        req = req.bearer_auth(api_key);
    }
    stream_chat(req, on_chunk)
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
        }
    }
    let out = acc.trim().to_string();
    if out.is_empty() {
        bail!("LLM 返回空内容");
    }
    Ok(out)
}

/// 把整段会话译文提炼成结构化重点（仅 LLM 引擎 openai/ollama）。
/// `on_chunk` 收到「截至目前的全量累计摘要」，流式回填前端。返回最终完整摘要。
pub fn summarize(
    client: &reqwest::blocking::Client,
    cfg: &RuntimeConfig,
    transcript: &str,
    mut on_chunk: impl FnMut(&str),
) -> Result<String> {
    let (base_url, api_key, model) = match cfg.translation_engine.as_str() {
        "ollama" => (cfg.ollama_base_url.as_str(), "", cfg.ollama_model.as_str()),
        _ => (
            cfg.llm_base_url.as_str(),
            cfg.llm_api_key.as_str(),
            cfg.llm_model.as_str(),
        ),
    };
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
         你是视频会议的纪要助手。下面是一段实时转录的双语记录（[原] 原文 / [译] 译文，可能不完整、有口语冗余、识别有误）。\
         请用「{lang_native}」提炼。\
         只保留信息量高的内容，去掉寒暄、重复、口头禅。\
         覆盖要求（最重要）：先通读整段记录，识别出每一个独立话题/议题，后面各节必须覆盖全部议题，一个都不能漏；\
         切勿仅因为两段共用了某个词就把两个不同话题合并，不确定时宁可拆开。\
         归并要求：同一个议题的信息往往分散在记录的不同位置（开头提一句、中间补充、结尾又回扣），\
         要把属于同一议题的所有分散内容归纳到同一条（或同一议题分组）要点里，不要让同一议题拆成多条零碎要点。\
         可读性要求（同样重要）：要点要短、突出重点，不要写成又长又满的句子。\
         每条要点用「**关键词/结论**：一句话说明」的格式，关键词用 Markdown 加粗，让人扫一眼就抓住重点。\
         凡是出现的具体数字（金额、比例、票数、年份、时间、人数、幅度等）都要原样保留，但融进简短说明里，不要堆砌。\
         不同要点之间、不同小节之间不要重复同一件事。\
         严格按以下 Markdown 结构输出（标题与正文一律用 {lang_native} 书写，层级保持不变）：\n\
         ## {h_overview}\n（2-3 句话总览，点到本段涉及的每个议题，简明扼要）\n\
         ## {h_points}\n- 每个议题至少一条，按「**关键词**：简述」格式，一条一个重点，不重复\n\
         ## {h_decisions}\n- 已拍板的结论（不要重复讨论要点的细节）；没有就写「{none_word}」\n\
         ## {h_actions}\n- 待办事项，尽量标注负责人和时间；没有就写「{none_word}」\n\
         ## {h_followup}\n- 悬而未决、需进一步确认的点；没有就写「{none_word}」\n\
         不要逐句翻译，不要编造记录中没有的信息，识别明显有误处可合理推断但不要臆造细节。\n\
         再次强调：最终输出从头到尾只能是 {lang_native}。",
    );
    let messages = vec![
        serde_json::json!({ "role": "system", "content": system }),
        serde_json::json!({ "role": "user", "content": transcript }),
    ];
    let body = serde_json::json!({
        "model": model,
        "temperature": 0.3,
        "stream": true,
        "messages": messages
    });
    let mut req = client.post(url).json(&body);
    if !api_key.is_empty() {
        req = req.bearer_auth(api_key);
    }
    stream_chat(req, &mut on_chunk)
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
        _ => {
            if allow_auto {
                "auto"
            } else {
                "en"
            }
        }
    }
}
