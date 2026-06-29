//! Qwen3-ASR-0.6B-int8 离线 POC：验证准确率 + 纯 CPU 逐段解码延迟。
//!
//! 用法：
//!   qwen3-asr-poc <模型目录> <wav1> [wav2 ...] [--threads N] [--hotwords "词A,词B"]
//!
//! 模型目录 = 解压后的 sherpa-onnx-qwen3-asr-0.6B-int8-2026-03-25/
//! 内含：conv_frontend.onnx / encoder.int8.onnx / decoder.int8.onnx / tokenizer/
//!
//! 关注两件事：
//!   1) text 是否把专有名词/同音词识别对（对照 SenseVoice 的错误）；
//!   2) RTF（解码耗时 / 音频时长）—— 这台无 N 卡，纯 CPU，决定能否实时。

use std::path::Path;
use std::time::Instant;

use sherpa_onnx::{
    OfflineQwen3ASRModelConfig, OfflineRecognizer, OfflineRecognizerConfig,
    OfflineSenseVoiceModelConfig, Wave,
};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!(
            "用法: {} <模型目录> <wav1> [wav2 ...] [--engine qwen3|sensevoice] [--lang auto|ja|zh|en] [--threads N] [--hotwords \"词A,词B\"]",
            args.get(0).map(|s| s.as_str()).unwrap_or("qwen3-asr-poc")
        );
        std::process::exit(2);
    }

    let model_dir = Path::new(&args[1]);

    // 解析可选参数 + 收集 wav 列表
    let mut wavs: Vec<String> = Vec::new();
    let mut threads: i32 = 4;
    let mut hotwords: Option<String> = None;
    let mut engine = String::from("qwen3"); // qwen3 | sensevoice
    let mut lang = String::from("auto"); // 仅 sensevoice 用：auto/zh/en/ja/ko/yue
    let mut i = 2;
    while i < args.len() {
        match args[i].as_str() {
            "--threads" => {
                threads = args
                    .get(i + 1)
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(4);
                i += 2;
            }
            "--hotwords" => {
                hotwords = args.get(i + 1).cloned();
                i += 2;
            }
            "--engine" => {
                engine = args.get(i + 1).cloned().unwrap_or_else(|| "qwen3".into());
                i += 2;
            }
            "--lang" => {
                lang = args.get(i + 1).cloned().unwrap_or_else(|| "auto".into());
                i += 2;
            }
            other => {
                wavs.push(other.to_string());
                i += 1;
            }
        }
    }
    if wavs.is_empty() {
        eprintln!("错误：未指定任何 wav 文件");
        std::process::exit(2);
    }

    // 按引擎组装识别器配置；解码循环两者完全一致。
    let mut config = OfflineRecognizerConfig::default();
    match engine.as_str() {
        "sensevoice" => {
            // SenseVoice：model_dir 下需有 model.int8.onnx + tokens.txt
            let model = model_dir.join("model.int8.onnx");
            let tokens = model_dir.join("tokens.txt");
            for p in [&model, &tokens] {
                if !p.exists() {
                    eprintln!("错误：缺少 SenseVoice 文件 {}", p.display());
                    std::process::exit(1);
                }
            }
            println!("加载 SenseVoice（lang={lang}, num_threads={threads}）…");
            config.model_config.sense_voice = OfflineSenseVoiceModelConfig {
                model: Some(model.to_string_lossy().into_owned()),
                language: Some(lang.clone()),
                use_itn: true,
            };
            config.model_config.tokens = Some(tokens.to_string_lossy().into_owned());
        }
        _ => {
            // Qwen3：四组件 + 可选热词
            let conv_frontend = model_dir.join("conv_frontend.onnx");
            let encoder = model_dir.join("encoder.int8.onnx");
            let decoder = model_dir.join("decoder.int8.onnx");
            let tokenizer = model_dir.join("tokenizer"); // 注意：是目录
            for p in [&conv_frontend, &encoder, &decoder, &tokenizer] {
                if !p.exists() {
                    eprintln!("错误：缺少模型文件 {}", p.display());
                    std::process::exit(1);
                }
            }
            println!("加载 Qwen3-ASR-0.6B-int8（num_threads={threads}）…");
            if let Some(hw) = &hotwords {
                println!("热词: {hw}");
            }
            config.model_config.qwen3_asr = OfflineQwen3ASRModelConfig {
                conv_frontend: Some(conv_frontend.to_string_lossy().into_owned()),
                encoder: Some(encoder.to_string_lossy().into_owned()),
                decoder: Some(decoder.to_string_lossy().into_owned()),
                tokenizer: Some(tokenizer.to_string_lossy().into_owned()),
                max_total_len: 512,
                max_new_tokens: 512,
                temperature: 1e-6,
                top_p: 0.8,
                seed: 42,
                hotwords: hotwords.clone(),
                ..Default::default()
            };
        }
    }
    config.model_config.num_threads = threads;

    let t_load = Instant::now();
    let recognizer = match OfflineRecognizer::create(&config) {
        Some(r) => r,
        None => {
            eprintln!("创建识别器失败（模型损坏或路径不对？）");
            std::process::exit(1);
        }
    };
    println!("识别器就绪，用时 {:.3}s\n", t_load.elapsed().as_secs_f64());

    let mut total_audio = 0.0_f64;
    let mut total_decode = 0.0_f64;

    for wav in &wavs {
        let wave = match Wave::read(wav) {
            Some(w) => w,
            None => {
                eprintln!("[跳过] 无法读取 {wav}");
                continue;
            }
        };
        let samples = wave.samples();
        let sr = wave.sample_rate();
        let audio_secs = samples.len() as f64 / sr as f64;

        let t = Instant::now();
        let stream = recognizer.create_stream();
        stream.accept_waveform(sr, samples);
        recognizer.decode(&stream);
        let text = stream
            .get_result()
            .map(|r| r.text.trim().to_string())
            .unwrap_or_default();
        let decode_secs = t.elapsed().as_secs_f64();

        let rtf = if audio_secs > 0.0 {
            decode_secs / audio_secs
        } else {
            0.0
        };
        total_audio += audio_secs;
        total_decode += decode_secs;

        println!("─ {wav}");
        println!("  音频 {audio_secs:.2}s | 解码 {decode_secs:.2}s | RTF {rtf:.3}");
        println!("  → {text}\n");
    }

    if total_audio > 0.0 {
        println!(
            "汇总：音频 {total_audio:.1}s，解码 {total_decode:.1}s，平均 RTF {:.3}",
            total_decode / total_audio
        );
    }
}
