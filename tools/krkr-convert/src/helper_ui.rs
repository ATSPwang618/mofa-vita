use inquire::{Confirm, InquireError, Select, Text, validator::Validation};
use krkr_convert::{
    archive, at9,
    helper::{Prepared, Target, hardware_counts, require_normalized},
    media, psv,
};
use std::{
    io::IsTerminal,
    path::{Path, PathBuf},
    process::Command,
};

type Result<T> = std::result::Result<T, InquireError>;
fn failure(error: String) -> InquireError {
    InquireError::Custom(error.into())
}
fn path(value: &str) -> PathBuf {
    let value = value.trim();
    PathBuf::from(
        value
            .strip_prefix('"')
            .and_then(|s| s.strip_suffix('"'))
            .or_else(|| value.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')))
            .unwrap_or(value),
    )
}
fn directory(default: &str) -> Result<PathBuf> {
    Text::new("游戏或资源目录：")
        .with_default(default)
        .with_help_message("可以粘贴或拖入带引号的路径；Esc 取消")
        .with_validator(|value: &str| {
            Ok(if path(value).is_dir() {
                Validation::Valid
            } else {
                Validation::Invalid("请输入已有目录".into())
            })
        })
        .prompt()
        .map(|s| path(&s))
}
fn filter(source: &Path) -> Result<Option<archive::FilterOptions>> {
    let detected = source.join("xp3filter.tjs");
    let choices = if detected.is_file() {
        vec![
            "使用检测到的 xp3filter.tjs",
            "明文 XP3，不使用过滤器",
            "指定其他过滤脚本",
        ]
    } else {
        vec!["明文 XP3，不使用过滤器", "指定其他过滤脚本"]
    };
    let choice = Select::new("XP3 解包方式：", choices).prompt()?;
    if choice == "明文 XP3，不使用过滤器" {
        return Ok(None);
    }
    let script = if choice == "使用检测到的 xp3filter.tjs" {
        detected
    } else {
        path(
            &Text::new("xp3filter.tjs 路径：")
                .with_validator(|s: &str| {
                    Ok(if path(s).is_file() {
                        Validation::Valid
                    } else {
                        Validation::Invalid("过滤脚本不存在".into())
                    })
                })
                .prompt()?,
        )
    };
    let bytes = std::fs::read(&script).map_err(|e| failure(e.to_string()))?;
    let encoding = if bytes.starts_with(&[0xff, 0xfe])
        || bytes.starts_with(&[0xfe, 0xff])
        || std::str::from_utf8(&bytes).is_ok()
    {
        "utf-8"
    } else {
        "shift-jis"
    };
    let encoding = Text::new("过滤脚本编码：")
        .with_default(encoding)
        .with_help_message("BOM 自动识别；无 BOM 的日文脚本通常为 shift-jis，可改为 gbk 等编码")
        .prompt()?;
    Ok(Some(archive::FilterOptions {
        script: Some(script),
        root: Some(source.to_owned()),
        encoding,
    }))
}
fn sibling(source: &Path, suffix: &str) -> PathBuf {
    let mut name = source.file_name().unwrap_or_default().to_owned();
    name.push(suffix);
    source.with_file_name(name)
}
fn display(path: &Path) -> String {
    let value = path.display().to_string();
    if let Some(rest) = value.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{rest}")
    } else {
        value.strip_prefix(r"\\?\").unwrap_or(&value).to_owned()
    }
}
fn tools_ready(tools: &mut media::Tools) -> Result<()> {
    for (name, program) in [
        ("FFmpeg", &mut tools.ffmpeg),
        ("FFprobe", &mut tools.ffprobe),
    ] {
        while !Command::new(&*program)
            .arg("-version")
            .output()
            .is_ok_and(|o| o.status.success())
        {
            eprintln!("未找到可运行的 {name}。");
            *program = path(&Text::new(&format!("{name} 可执行文件路径：")).prompt()?);
        }
    }
    Ok(())
}
fn canvas(choices: &[krkr_protocol::graphics::Size]) -> Result<krkr_protocol::graphics::Size> {
    let hint = if choices.len() == 1 {
        format!("{}x{}", choices[0].width, choices[0].height)
    } else {
        String::new()
    };
    if choices.len() > 1 {
        println!(
            "发现不同画布配置：{}",
            choices
                .iter()
                .map(|s| format!("{}x{}", s.width, s.height))
                .collect::<Vec<_>>()
                .join("、")
        );
    }
    let mut prompt = Text::new("游戏原始画布尺寸：")
        .with_help_message(
            "读取 Config.tjs 的 scWidth/scHeight；无法确定时请输入原始分辨率，例如 1280x720",
        )
        .with_validator(|s: &str| {
            Ok(match psv::dimensions(s) {
                Ok(_) => Validation::Valid,
                Err(_) => Validation::Invalid("请输入有效的 宽x高，例如 1280x720".into()),
            })
        });
    if !hint.is_empty() {
        prompt = prompt.with_default(&hint);
    }
    psv::dimensions(&prompt.prompt()?).map_err(failure)
}

fn patterns(kind: &str, inventories: &[media::Report]) -> Result<Vec<String>> {
    let examples: Vec<_> = inventories
        .iter()
        .flat_map(|r| &r.entries)
        .filter(|e| e.media.as_ref().is_some_and(|m| m.kind == kind))
        .take(8)
        .map(|e| e.path.as_str())
        .collect();
    println!("资源路径示例：{}", examples.join("、"));
    println!(
        "按转换前的相对路径匹配，应用于松散文件和每个 XP3 内部；例如 bg/*、bgm/*.ogg。路径分隔符用 /。"
    );
    let mut result = Vec::new();
    loop {
        result.push(
            Text::new("匹配规则：")
                .with_help_message(
                    "一次输入一个规则；* 匹配任意路径，包括子目录；文件名中的 [ 写作 [[]",
                )
                .with_validator(|s: &str| {
                    Ok(if s.trim().is_empty() {
                        Validation::Invalid("请输入资源路径或匹配规则".into())
                    } else {
                        match glob::Pattern::new(s.trim()) {
                            Ok(_) => Validation::Valid,
                            Err(e) => Validation::Invalid(e.to_string().into()),
                        }
                    })
                })
                .prompt()?
                .trim()
                .to_owned(),
        );
        if !Confirm::new("继续添加匹配规则？")
            .with_default(false)
            .prompt()?
        {
            return Ok(result);
        }
    }
}

fn hardware_options(inventories: &[media::Report], options: &mut psv::Options) -> Result<()> {
    let has_media = |kind: &str| {
        inventories.iter().flat_map(|r| &r.entries).any(|e| {
            e.media
                .as_ref()
                .is_some_and(|m| m.kind == kind && (kind != "audio" || m.container != "at9"))
        })
    };
    if has_media("audio") {
        loop {
            let choice = Select::new(
                "PSV 音频处理：",
                vec![
                    "全部音频转 AT9（硬件解码）",
                    "按路径选择 AT9 音频",
                    "保留常规音频策略",
                ],
            )
            .with_help_message("AT9 为有损编码，需要外部 at9tool；保留原采样时间线和循环点")
            .prompt()?;
            if choice == "保留常规音频策略" {
                options.at9 = None;
                break;
            }
            let globs = if choice == "全部音频转 AT9（硬件解码）" {
                vec!["*".into()]
            } else {
                patterns("audio", inventories)?
            };
            options.at9 = Some(at9::Options {
                tool: PathBuf::new(),
                globs,
            });
            match hardware_counts(inventories, options) {
                Ok(counts) => println!("选中 {} 个音频；编码后校验解码样本数。", counts.at9),
                Err(error) => {
                    eprintln!("音频选择无效：{error}");
                    continue;
                }
            }
            options.at9.as_mut().unwrap().tool =
                krkr_convert::encoders::audio().map_err(failure)?;
            break;
        }
    }
    if has_media("image") {
        loop {
            let choice = Select::new(
                "PSV 图片处理：",
                vec![
                    "自动筛选并压缩图片（含透明纹理）",
                    "手动指定图片范围",
                    "保留无损图片策略",
                ],
            )
            .with_help_message(
                "不透明图片使用 BC1，透明图片使用 BC3；颜色/透明度损失过大则保留无损，需要新版原生驱动",
            )
            .prompt()?;
            if choice == "保留无损图片策略" {
                options.texture_globs.clear();
                options.texture_auto = false;
                break;
            }
            options.texture_auto = choice == "自动筛选并压缩图片（含透明纹理）";
            options.texture_globs = if options.texture_auto {
                Vec::new()
            } else {
                patterns("image", inventories)?
            };
            match hardware_counts(inventories, options) {
                Ok(counts) => {
                    println!(
                        "候选图片 {} 张；保留图片标签，大图自动分块；跳过遮罩、规则/界面名称、小图及灰度图。编码后校验颜色和透明度，损失过大自动保留无损，原因写入转换摘要。",
                        counts.textures
                    );
                    if counts.textures != 0 {
                        let quality = Select::new(
                            "纹理编码质量：",
                            vec![
                                "平衡（推荐）：平衡体积、画质与编码时间",
                                "快速：减少编码搜索，不增加密度重试",
                                "高质量：增加搜索，转换更慢",
                            ],
                        )
                        .with_help_message(
                            "内置 Rust rgbcx，无需纹理 exe；颜色或透明度未过检时保留无损图片。",
                        )
                        .prompt()?;
                        let storage = Select::new(
                            "纹理存储：",
                            vec!["BC＋无损封装（推荐，快速转换）", "原生 BC（直接上传）"],
                        )
                        .prompt()?;
                        options.texture_storage = if storage.starts_with("BC＋") {
                            krkr_convert::bc::Storage::BcCrunch
                        } else {
                            krkr_convert::bc::Storage::Bc
                        };
                        options.texture_quality = if quality.starts_with("快速") {
                            krkr_convert::bc::Quality::Fast
                        } else if quality.starts_with("高质量") {
                            krkr_convert::bc::Quality::High
                        } else {
                            krkr_convert::bc::Quality::Balanced
                        };
                    }
                    break;
                }
                Err(error) => eprintln!("图片选择无效：{error}"),
            }
        }
    }
    Ok(())
}
pub fn run(
    source: Option<PathBuf>,
    tools: media::Tools,
    jobs: Option<u16>,
) -> std::result::Result<(), String> {
    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        return Err(
            "helper 需要交互终端；脚本或重定向环境请使用 xp3 / probe / adjust / psv 子命令".into(),
        );
    }
    match configure_workers(source, tools, jobs) {
        Ok(()) | Err(InquireError::OperationCanceled | InquireError::OperationInterrupted) => {
            Ok(())
        }
        Err(error) => Err(error.to_string()),
    }
}
fn configure_workers(
    source: Option<PathBuf>,
    tools: media::Tools,
    jobs: Option<u16>,
) -> Result<()> {
    let default = jobs
        .map(usize::from)
        .unwrap_or_else(crate::default_jobs)
        .to_string();
    let jobs = Text::new("并行任务数（1–64）：")
        .with_default(&default)
        .with_help_message("直接输入数字；默认按可用 CPU 线程数。任务越多，同时占用的内存越多。")
        .with_validator(|text: &str| {
            Ok(
                if text
                    .trim()
                    .parse::<u16>()
                    .is_ok_and(|n| (1..=64).contains(&n))
                {
                    Validation::Valid
                } else {
                    Validation::Invalid("请输入 1 到 64 的整数".into())
                },
            )
        })
        .prompt()?;
    let jobs = jobs
        .trim()
        .parse::<usize>()
        .map_err(|e| failure(e.to_string()))?;
    rayon::ThreadPoolBuilder::new()
        .num_threads(jobs)
        .build()
        .map_err(|e| failure(format!("cannot start worker pool: {e}")))?
        .install(|| interactive(source, tools))
}
fn interactive(source: Option<PathBuf>, mut tools: media::Tools) -> Result<()> {
    let goal = Select::new(
        "希望如何处理资源？",
        vec![
            "生成 PSV 游戏资源",
            "按实际格式归一化并建立资源 link（保持原始尺寸）",
            "解包 XP3",
            "封包 XP3",
        ],
    )
    .prompt()?;
    if goal == "生成 PSV 游戏资源" {
        println!("请选择已归一化的资源目录；此步骤只进行 PSV 转换，不再归一化或修复资源。");
    }
    let source = match source {
        Some(p) => p,
        None => directory(".")?,
    };
    let source = std::fs::canonicalize(&source).map_err(|e| failure(e.to_string()))?;
    if goal == "封包 XP3" {
        let each = Select::new(
            "封包范围：",
            vec!["整个目录打成一个 XP3", "下一级子目录分别封包"],
        )
        .prompt()?
            == "下一级子目录分别封包";
        if Confirm::new("开始封包？已有 XP3 不会覆盖。")
            .with_default(true)
            .prompt()?
        {
            archive::pack(
                &source,
                each,
                krkr_assets::xp3::Compression::Auto,
                krkr_convert::progress::println,
            )
            .map_err(failure)?;
        }
        return Ok(());
    }
    if goal == "解包 XP3" {
        let inputs = vec![source.join("*.xp3")];
        let expanded = archive::expand(&inputs).map_err(failure)?;
        let options = filter(&source)?;
        println!("找到 {} 个 XP3，输出到各包旁的同名目录。", expanded.len());
        if Confirm::new("开始解包？").with_default(true).prompt()? {
            archive::unpack_with_filter(
                &expanded,
                options.as_ref(),
                krkr_convert::progress::println,
            )
            .map_err(failure)?;
        }
        return Ok(());
    }
    tools_ready(&mut tools)?;
    let default = sibling(
        &source,
        if goal == "生成 PSV 游戏资源" {
            "-psv"
        } else {
            "-normalized"
        },
    );
    let output = path(
        &Text::new("输出目录：")
            .with_default(&display(&default))
            .with_help_message("生成新目录，保留原游戏；已有目录不会覆盖")
            .prompt()?,
    );
    let output = krkr_convert::helper::output_path(&source, &output).map_err(failure)?;
    let report_path = sibling(&output, ".convert.json");
    if report_path.exists() {
        return Err(failure(format!("报告已存在：{}", display(&report_path))));
    }
    let has_archives = contains_archives(&source).map_err(failure)?;
    let filter = if has_archives && goal != "生成 PSV 游戏资源" {
        filter(&source)?
    } else {
        None
    };
    println!("准备资源：{}", display(&source));
    let prepared = Prepared::new(&source, &output, filter.as_ref()).map_err(failure)?;
    let mut target = if goal == "生成 PSV 游戏资源" {
        Target::psv(canvas(&prepared.canvases().map_err(failure)?)?)
    } else {
        Target::Normalize
    };
    let inventories = prepared.inspect(&tools).map_err(failure)?;
    let files: usize = inventories.iter().map(|r| r.entries.len()).sum();
    let mismatches = inventories
        .iter()
        .flat_map(|r| &r.entries)
        .filter(|e| e.consistency == media::Consistency::Mismatch)
        .count();
    let unreadable: Vec<_> = inventories
        .iter()
        .flat_map(|r| &r.entries)
        .filter(|e| e.consistency == media::Consistency::Unreadable)
        .map(|e| e.path.as_str())
        .collect();
    if !unreadable.is_empty() {
        return Err(failure(format!(
            "无法读取的媒体：{}",
            unreadable.join("、")
        )));
    }
    println!(
        "共 {files} 个文件，{mismatches} 个格式不一致；并行任务 {}。",
        rayon::current_num_threads()
    );
    if matches!(target, Target::Psv(_)) {
        require_normalized(&inventories).map_err(failure)?;
        println!("使用已归一化资源及现有 link；跳过归一化，仅执行 PSV 转换。");
    }
    if matches!(target, Target::Normalize) {
        println!("按实际内容修正文件扩展名，旧名称通过内部资源 link 保留；游戏脚本不修改。");
        println!("TLG 标签统一为 UTF-8；即使媒体格式一致，也会检查旧编码标签。");
    }
    if let Target::Psv(options) = &mut target {
        println!(
            "XP3 封包：纹理和文本按 256 KiB 独立段无损快速压缩；收益不足保留原样，音视频直接存储。"
        );
        hardware_options(&inventories, options)?;
        let counts = hardware_counts(&inventories, options).map_err(failure)?;
        println!(
            "硬件资源转换：AT9 音频 {} 个，纹理候选 {} 张（按透明度自动选格式）。",
            counts.at9, counts.textures
        );
        println!(
            "其余资源沿用常规 PSV 策略：不支持的音频转为 Vorbis 并校验样本数，静态图片按需缩放/转为 PNG。"
        );
        println!(
            "画布 {}x{} → 适配 960x544；视频使用 H.264 Main / AAC，AMV 保留。",
            options.canvas.width, options.canvas.height
        );
    }
    println!(
        "输出：{}\n报告：{}",
        display(&output),
        display(&report_path)
    );
    if filter.is_some() {
        println!("过滤脚本只用于本次解包，输出中移除自动加载的根目录 xp3filter.tjs。");
    }
    if !Confirm::new("开始自动处理？").with_default(true).prompt()? {
        return Ok(());
    }
    let result = prepared
        .convert(inventories, target, &tools)
        .map_err(failure)?;
    super::json(&result, Some(&report_path)).map_err(failure)?;
    for part in &result.parts {
        if part.hardware_audio != 0
            || part.compressed_textures != 0
            || !part.texture_skips.is_empty()
        {
            println!(
                "已转换：{} / AT9 {} 个，BC1 {} 张，BC3 RGBA {} 张；保留无损 {} 张",
                part.archive.as_deref().unwrap_or("loose"),
                part.hardware_audio,
                part.compressed_textures - part.transparent_textures,
                part.transparent_textures,
                part.texture_skips.len()
            );
            if part.compressed_textures != 0 {
                println!(
                    "  纹理分块：无损封装 {} / 原生 BC {}；封包前 {:.1} MiB，GPU 块数据 {:.1} MiB（非全游戏运行内存）",
                    part.packed_tiles,
                    part.native_bc_tiles,
                    part.texture_encoded_bytes as f64 / 1048576.,
                    part.texture_gpu_bytes as f64 / 1048576.
                );
            }
            let mut reasons = std::collections::BTreeMap::new();
            for skip in &part.texture_skips {
                // Keep measurements per image in JSON, not hundreds of nearly
                // identical console lines differing only in RMSE or counts.
                let reason = if skip.detail.starts_with("compression ") {
                    skip.detail.split(" (").next().unwrap()
                } else {
                    &skip.detail
                };
                *reasons.entry(reason).or_insert(0usize) += 1;
            }
            for (reason, count) in reasons {
                println!("  保留无损 {count} 张：{reason}");
            }
        }
        for repair in &part.repairs {
            println!(
                "已修复：{} / {}：{}",
                part.archive.as_deref().unwrap_or("loose"),
                repair.path,
                repair.detail
            );
        }
    }
    println!("完成：{}", display(&result.output));
    Ok(())
}

fn contains_archives(source: &Path) -> std::result::Result<bool, String> {
    let mut pending = vec![source.to_owned()];
    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(&directory).map_err(|e| e.to_string())? {
            let entry = entry.map_err(|e| e.to_string())?;
            let kind = entry.file_type().map_err(|e| e.to_string())?;
            if kind.is_dir() {
                pending.push(entry.path());
            } else if kind.is_file()
                && entry
                    .path()
                    .extension()
                    .is_some_and(|e| e.eq_ignore_ascii_case("xp3"))
            {
                return Ok(true);
            }
        }
    }
    Ok(false)
}
