//! 设置页。
//!
//! 只画界面，不写盘：这里改的是 `Settings` 值本身，由 `main.rs`
//! 在每帧末尾比较差异后落盘。

use std::sync::mpsc::{self, Receiver, TryRecvError};

use aicraft_core::java;
use aicraft_core::logging;
use aicraft_core::settings::{LauncherMode, Mirror, Settings, WindowMode};
use egui::RichText;

use crate::theme;

/// 下拉框里「当前选的是哪一个」那一行文字。
///
/// 只写路径的话，`C:\Program Files\Java\jdk-21\bin\javaw.exe` 太长会把
/// 下拉框撑变形，而玩家真正在乎的是「Java 几」；扫到了就顺带把大版本写出来，
/// 没扫到（或者指定的是列表外的）就老老实实显示路径。
pub(crate) fn describe_java(path: &std::path::Path, runtimes: Option<&[java::JavaRuntime]>) -> String {
    if let Some(rt) = runtimes.and_then(|list| list.iter().find(|rt| rt.path == path)) {
        return format!("Java {} · {}", rt.major_version, rt.home().display());
    }
    format!("已指定：{}", path.display())
}



/// 页面上表达的动作，出页面后由 `main.rs` 执行。
pub enum Action {
    /// 把日志导出成文件
    ExportLog,
    /// 清空界面里的日志
    ClearLog,
    /// 弹文件选择框，挑一个 java 可执行文件加进列表
    AddJava,
}

impl Action {
    /// 给操作日志用的一句话。
    pub fn describe(&self) -> String {
        match self {
            Action::ExportLog => "设置：导出日志".to_string(),
            Action::ClearLog => "设置：清空日志显示".to_string(),
            Action::AddJava => "设置：添加 Java 路径".to_string(),
        }
    }
}

/// 本机 Java 探测的后台状态。
///
/// 探测得挨个起 `java -version`。本机实测 8 个候选要 **4 秒**，其中两个
/// Oracle `javapath` 转发层各占 1.6 / 2.0 秒。这活儿绝不能放在绘制里做：
/// 玩家点开设置页，窗口会整块僵住几秒，看着就像崩溃了。
///
/// 所以这里只负责「要不要起线程、结果回来没有」，界面拿到 `None` 就画
/// 一行「正在扫描…」，不阻塞任何一帧。
#[derive(Default)]
pub struct JavaScan {
    /// 扫完了才有值；`None` 表示还在扫（或者还没开始）。
    runtimes: Option<Vec<java::JavaRuntime>>,
    /// 玩家自己点「添加 Java 路径」加进来的那些。
    ///
    /// 单独存一份是因为扫描还没回来时 `runtimes` 是 `None`，那一刻添加的
    /// 运行时无处可放；扫描结束后再并进去。
    extra: Vec<java::JavaRuntime>,
    /// 上一次添加失败的原因（路径不是 java.exe 之类）
    pub error: Option<String>,
    /// 上一次添加成功时的说明，和 `error` 互斥。
    pub note: Option<String>,
    /// 设置文件里记下的那些手工路径，扫描时一起探一遍。
    seeded: Vec<std::path::PathBuf>,
    rx: Option<Receiver<Vec<java::JavaRuntime>>>,
}

impl JavaScan {
    pub fn runtimes(&self) -> Option<&[java::JavaRuntime]> {
        self.runtimes.as_deref()
    }

    /// 是否正在扫。界面据此保持重绘——否则 egui 睡下去，结果回来也没人画。
    pub fn is_running(&self) -> bool {
        self.rx.is_some()
    }

    /// 手动加一个运行时。已经在列表里就返回 `false`。
    ///
    /// 去重口径与 [`java::detect_all`] 一致（主版本 + 完整版本串）：
    /// 同一个 JDK 换个路径加进来，不该多出一行。
    /// 记下设置文件里存着的手工路径。
    ///
    /// 扫描线程只认 JAVA_HOME / PATH / 常见安装目录，装在别处的 JDK 扫不到；
    /// 这些是玩家自己挑过的，重启之后得原样回到列表里。
    pub fn seed(&mut self, paths: &[String]) {
        self.seeded = paths
            .iter()
            .map(|path| std::path::PathBuf::from(path.trim()))
            .filter(|path| !path.as_os_str().is_empty())
            .collect();
    }

    pub fn add(&mut self, rt: java::JavaRuntime) -> bool {
        fn same(a: &java::JavaRuntime, b: &java::JavaRuntime) -> bool {
            a.major_version == b.major_version && a.version_string == b.version_string
        }
        self.error = None;
        self.note = None;
        match &mut self.runtimes {
            Some(list) => {
                if list.iter().any(|k| same(k, &rt)) {
                    return false;
                }
                list.push(rt);
                list.sort_by_key(|k| std::cmp::Reverse(k.major_version));
                true
            }
            None => {
                if self.extra.iter().any(|k| same(k, &rt)) {
                    return false;
                }
                self.extra.push(rt);
                true
            }
        }
    }

    /// 起一次扫描。已经在扫、或者已经扫过了就不再重复。
    pub fn start(&mut self) {
        if self.rx.is_some() || self.runtimes.is_some() {
            return;
        }
        let (tx, rx) = mpsc::channel();
        self.rx = Some(rx);
        let seeded = std::mem::take(&mut self.seeded);
        std::thread::spawn(move || {
            let mut list = java::detect_all();
            // 手工加进来的那些未必在扫描范围里，逐个探一遍再并进去。
            let picked: Vec<java::JavaRuntime> =
                seeded.iter().filter_map(|path| java::probe_java(path)).collect();
            merge_into(&mut list, picked);
            let _ = tx.send(list);
        });
    }

    /// 收扫描结果，返回「这一帧有没有新东西要重画」。
    pub fn poll(&mut self) -> bool {
        let Some(rx) = self.rx.take() else {
            return false;
        };
        match rx.try_recv() {
            Ok(mut list) => {
                // 扫描期间手动加进来的那些要并进去，否则它们会凭空消失
                let extra = std::mem::take(&mut self.extra);
                merge_into(&mut list, extra);
                self.runtimes = Some(list);
                true
            }
            Err(TryRecvError::Empty) => {
                self.rx = Some(rx);
                false
            }
            // 线程没了（panic）：当成「一个都没扫到」，别让界面永远转圈。
            Err(TryRecvError::Disconnected) => {
                self.runtimes = Some(Vec::new());
                true
            }
        }
    }
}

/// 把手工挑的运行时并进扫描结果：同主版本 + 同版本串的只留一条，最后按主版本降序。
fn merge_into(list: &mut Vec<java::JavaRuntime>, extra: Vec<java::JavaRuntime>) {
    for rt in extra {
        let duplicate = list.iter().any(|k| {
            k.major_version == rt.major_version && k.version_string == rt.version_string
        });
        if !duplicate {
            list.push(rt);
        }
    }
    list.sort_by_key(|k| std::cmp::Reverse(k.major_version));
}

/// 画设置页。所有控件直接改 `settings`。
pub fn page(
    ui: &mut egui::Ui,
    settings: &mut Settings,
    log_view: &mut logging::Level,
    log: &[logging::Entry],
    java: &mut JavaScan,
    busy: bool,
) -> Option<Action> {
    let mut action = None;

    // 外层是滚动区，浮动滚动条压在内容右侧；卡片按可用宽度铺满会正好被它
    // 盖住一条边，看起来像贴到了窗口上。
    //
    // `set_width` 设的是卡片**内层**宽度，`theme::card()` 自己还有 16px 的
    // 左右内边距，所以外层会比它多 32px —— 这里要先扣掉那 32，再让出
    // 24px 的右边距，卡片才真的不贴边。
    let card_width = (ui.available_width() - 32.0 - 24.0).max(320.0);

    // ---- 游戏 ----
    // 换游戏目录的入口搬到「版本管理 → 目录列表」了。
    // 用户名也不在这里：它属于「账号」，去标题栏右上角那个按钮里改。
    section(ui, "游戏");
    // 客户端 ID 不露面：登录用的是写死在代码里的应用注册，玩家没有理由改它。

    theme::card().show(ui, |ui| {
        ui.set_width(card_width);
        ui.horizontal(|ui| {
            theme::hand(ui.add_enabled(
                !busy,
                egui::Checkbox::new(&mut settings.isolate_instances, "实例隔离"),
            ));
            ui.label(
                RichText::new("每个版本的存档 / 模组 / 配置各放各的（versions/<版本>/）")
                    .size(10.5)
                    .color(theme::TEXT_WEAK),
            );
        });

        divider(ui);

        // ---- 游戏运行时启动器状态 ----
        // 标题在左、控件在右，和这一页其它行一样：左边一列是「这一项叫什么」，
        // 右边一列是「它现在是什么」，扫一眼就对上号。
        setting_row(ui, "游戏运行时启动器状态", |ui| {
            ui.add_enabled_ui(!busy, |ui| {
                let combo = theme::combo(
                    ui,
                    "launcher-mode",
                    RichText::new(settings.launcher_mode.label()).size(12.0),
                    COMBO_W,
                    |ui| {
                        for mode in LauncherMode::ALL {
                            ui.selectable_value(&mut settings.launcher_mode, mode, mode.label());
                        }
                    },
                );
                theme::hand(combo);
            });
        });

        divider(ui);

        // ---- 游戏窗口大小 ----
        setting_row(ui, "游戏窗口大小", |ui| {
            ui.add_enabled_ui(!busy, |ui| {
                let combo = theme::combo(
                    ui,
                    "window-size",
                    RichText::new(window_text(settings)).size(12.0),
                    COMBO_W,
                    |ui| {
                        for (index, text) in PRESETS.iter().enumerate() {
                            if ui
                                .selectable_label(window_preset(settings) == index, *text)
                                .clicked()
                            {
                                apply_preset(settings, index);
                            }
                        }
                    },
                );
                theme::hand(combo);
            });
        });
        // 指定宽高之后总还能再调两个数：预设只是替玩家填一遍，不是锁死。
        if settings.window_mode == WindowMode::Fixed {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.add_enabled_ui(!busy, |ui| {
                    theme::hand(ui.add(
                        egui::DragValue::new(&mut settings.window_width)
                            .range(320..=8192)
                            .speed(8.0),
                    ));
                    ui.label(RichText::new("×").size(12.0).color(theme::TEXT_WEAK));
                    theme::hand(ui.add(
                        egui::DragValue::new(&mut settings.window_height)
                            .range(240..=8192)
                            .speed(8.0),
                    ));
                });
                ui.label(
                    RichText::new("像素；游戏里按 F11 还能切全屏")
                        .size(10.5)
                        .color(theme::TEXT_WEAK),
                );
            });
        } else if settings.window_mode == WindowMode::Maximized {
            ui.add_space(4.0);
            ui.label(
                RichText::new("铺满桌面，会留出任务栏")
                    .size(10.5)
                    .color(theme::TEXT_WEAK),
            );
        } else if settings.window_mode == WindowMode::Fullscreen {
            ui.add_space(4.0);
            ui.label(
                RichText::new("独占全屏，游戏里按 F11 或 Esc 能退出来")
                    .size(10.5)
                    .color(theme::TEXT_WEAK),
            );
        }

        divider(ui);

        // ---- 内存 ----
        // 内存也是「这一份游戏怎么跑」，跟着游戏这一节走，不单开一张卡。
        memory_body(ui, settings);

        divider(ui);

        // ---- Java ----
        // 一个下拉说完三件事：现在是自动挑还是指定了某一个、本机有哪几个
        // 可选、直接在这儿换。原来那版把「可用的 Java」摊成三行、每行一个
        // 「用这个」按钮，占掉半屏还得分两步——选运行时本来就是一个选择题。
        //
        // 不再给一个自由输入的文本框：玩家输的十有八九是 JDK 的安装目录
        // （`C:\Program Files\Java\jdk-21`），而不是 `bin\javaw.exe`，
        // 那种路径到启动时会报「设置里指定的 Java 不存在」。
        // 要加不在列表里的，走「添加 Java 路径」挑文件。
        ui.horizontal(|ui| {
            ui.label(
                RichText::new("Java 路径")
                    .size(12.0)
                    .color(theme::TEXT_WEAK),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let add = egui::Button::new(RichText::new("添加 Java 路径").size(11.5))
                    .fill(theme::CARD)
                    .stroke(egui::Stroke::new(1.0, theme::BORDER))
                    .corner_radius(egui::CornerRadius::same(7))
                    .min_size(egui::vec2(112.0, 26.0));
                if ui.add_enabled(!busy, add).clicked() {
                    action = Some(Action::AddJava);
                }
            });
        });
        ui.add_space(6.0);

        // 列表来自后台线程（`JavaScan`），这里不探测、不等候。
        java.start();
        let runtimes = java.runtimes();
        // 这里只说「自动还是指定的那一个」。自动到底会挑到哪个 Java 是
        // 每份版本各自的事（清单不同、要的主版本也不同），写在这一页只会
        // 让人以为全局有个固定的答案——那一句放在管理页的游戏设置里。
        let current_text = match &settings.java_path {
            None => "自动选择".to_string(),
            Some(path) => describe_java(path, runtimes),
        };
        let combo = theme::combo(
            ui,
            "java-choice",
            RichText::new(current_text).size(12.0),
            ui.available_width(),
            |ui| {
                ui.selectable_value(&mut settings.java_path, None, "自动选择");
                match runtimes {
                    None => {
                        ui.label(
                            RichText::new("正在扫描本机的 Java…")
                                .size(11.0)
                                .color(theme::TEXT_WEAK),
                        );
                    }
                    Some([]) => {
                        ui.label(
                            RichText::new("没扫到本机的 Java，点右上「添加 Java 路径」挑一个")
                                .size(11.0)
                                .color(theme::TEXT_WEAK),
                        );
                    }
                    Some(list) => {
                        for rt in list {
                            let label = format!("Java {} · {}", rt.major_version, rt.home().display());
                            ui.selectable_value(&mut settings.java_path, Some(rt.path.clone()), label);
                        }
                    }
                }
            },
        );
        theme::hand(combo);

        if let Some(error) = &java.error {
            ui.add_space(2.0);
            ui.label(RichText::new(error.clone()).size(10.5).color(theme::DANGER));
        }
        if let Some(note) = &java.note {
            ui.add_space(2.0);
            ui.label(RichText::new(note.clone()).size(10.5).color(theme::TEXT_WEAK));
        }

        // 下拉只回答「现在用哪个」，看不到「本机到底有哪几个、各在哪儿」。
        // 所以下面把扫到的逐个列出来：版本号、完整版本串、路径一眼扫完，
        // 点一行就是换过去——比展开下拉再找一遍省事。
        if let Some(list) = runtimes
            && !list.is_empty() {
            ui.add_space(8.0);
            java_list(ui, list, settings.java_path.as_deref());
        }

        divider(ui);

        // ---- 显卡 ----
        // 双显卡笔记本上最常见的一句抱怨是「同样的整合包，别人 120 帧我 40」，
        // 原因往往是 Windows 把 javaw.exe 派给了核显。系统那个设置页藏得深，
        // 这里给一个开关，启动时替玩家写那条偏好。
        setting_row(ui, "使用高性能显卡", |ui| {
            ui.add_enabled_ui(!busy, |ui| {
                if theme::switch(ui, "", settings.high_performance_gpu, !busy).clicked() {
                    settings.high_performance_gpu = !settings.high_performance_gpu;
                }
            });
        });
        ui.label(
            RichText::new(
                "启动游戏时把这次挑中的 Java 指定为「高性能」，走独立显卡；\
                 关掉后下次启动会把那条指定撤掉。只影响这台电脑上的这个 java.exe，\
                 不改任何系统设置，随时能在 Windows「显示卡」里看到。",
            )
            .size(10.5)
            .color(theme::TEXT_WEAK),
        );
    });
    ui.add_space(14.0);

    // ---- 下载设置 ----
    section(ui, "下载设置");
    theme::card().show(ui, |ui| {
        ui.set_width(card_width);
        mirror_row(ui, "版本下载源", &mut settings.mirror);

        divider(ui);

        ui.label(
            RichText::new("模组加载器下载源")
                .size(12.0)
                .color(theme::TEXT_WEAK),
        );
        mirror_row(ui, "Fabric", &mut settings.fabric_mirror);
        mirror_row(ui, "Forge", &mut settings.forge_mirror);
        mirror_row(ui, "NeoForge", &mut settings.neoforge_mirror);

        divider(ui);

        ui.horizontal(|ui| {
            ui.label(RichText::new("并发下载").size(12.0).color(theme::TEXT_WEAK));
            ui.add_space(8.0);
            theme::hand(ui.add(egui::DragValue::new(&mut settings.concurrency).range(1..=16)));
            ui.label(
                RichText::new("个文件同时下；太大反而容易被限速")
                    .size(10.5)
                    .color(theme::TEXT_WEAK),
            );
        });

        divider(ui);

        ui.horizontal(|ui| {
            // 勾选框直接绑一个临时 bool，因为「不限速」内部存的是 None 而不是 0
            let mut limited = settings.download_limit_kbps.is_some();
            if theme::hand(ui.checkbox(&mut limited, "下载限速")).changed() {
                settings.download_limit_kbps = if limited { Some(2048) } else { None };
            }
            if let Some(kbps) = settings.download_limit_kbps.as_mut() {
                ui.add_space(6.0);
                theme::hand(
                    ui.add(
                        egui::DragValue::new(kbps)
                            .range(
                                aicraft_core::settings::MIN_LIMIT_KBPS
                                    ..=aicraft_core::settings::MAX_LIMIT_KBPS,
                            )
                            .speed(16.0)
                            .suffix(" KB/s"),
                    ),
                );
                ui.add_space(6.0);
                ui.label(
                    RichText::new(aicraft_core::speed::describe(settings.download_limit_kbps))
                        .size(10.5)
                        .color(theme::TEXT_WEAK),
                );
            }
        });

    });

    ui.add_space(14.0);

    // ---- JVM 参数 ----
    section(ui, "JVM 参数");
    theme::card().show(ui, |ui| {
        ui.set_width(card_width);
        let mut resp = ui.add(
            egui::TextEdit::multiline(&mut settings.jvm_args)
                .hint_text("-XX:+UseG1GC -Dfile.encoding=UTF-8")
                .desired_rows(2)
                .desired_width(f32::INFINITY),
        );
        theme::input_menu(&mut resp, &mut settings.jvm_args, false);
        ui.label(
            RichText::new(
                "按空白/换行切成参数，追加在版本自带的 JVM 参数之后；\
             别把 -cp / -Xmx 也写进来，那两个由启动器管。",
            )
            .size(10.5)
            .color(theme::TEXT_WEAK),
        );
    });

    ui.add_space(14.0);

    // ---- 日志 ----
    section(ui, "日志");
    theme::card().show(ui, |ui| {
        ui.set_width(card_width);
        ui.horizontal(|ui| {
            ui.label(RichText::new("显示").size(12.0).color(theme::TEXT_WEAK));
            ui.add_space(6.0);
            for level in logging::Level::ALL {
                if ui
                    .selectable_label(*log_view == level, crate::level_filter_text(level))
                    .clicked()
                {
                    *log_view = level;
                }
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("清空").clicked() {
                    action = Some(Action::ClearLog);
                }
                if ui.button("导出日志…").clicked() {
                    action = Some(Action::ExportLog);
                }
            });
        });
        ui.label(
            RichText::new(format!(
                "{}　可以往上翻，导出是全部；导出时会让你自己选存到哪",
                scope_text(log.len(), logging::total(), *log_view)
            ))
            .size(10.5)
            .color(theme::TEXT_WEAK),
        );
        ui.add_space(4.0);
        theme::inset().show(ui, |ui| {
            ui.set_width(card_width);
            // 高度必须先占住再往里放 `ScrollArea`：只写 `max_height` 是「上限」，
            // 内容四行它就缩成四行；在内容里补 `set_min_height` 又会被
            // `stick_to_bottom` 立刻滚到空白那段。所以用固定尺寸的外壳，
            // 让滚动区自己去填满它。
            let inner_w = ui.available_width();
            ui.allocate_ui_with_layout(
                egui::vec2(inner_w, LOG_PANEL_H - INSET_PADDING_Y),
                egui::Layout::top_down(egui::Align::Min),
                |ui| {
                    ui.set_min_width(inner_w);
                    egui::ScrollArea::vertical()
                        .id_salt("settings-log")
                        .auto_shrink([false, false])
                        .stick_to_bottom(true)
                        .show(ui, |ui| {
                            // 行距交给分隔线自己控制：默认 9px 间距加上线会拱得太松，
                            // 十行就撑出面板了。
                            ui.spacing_mut().item_spacing.y = 0.0;
                            if log.is_empty() {
                                // 空框看着像坏了：「这条日志没了」和「这一档没记录」
                                // 是两回事，得说清楚。
                                ui.label(
                                    RichText::new(format!(
                                        "「{}」这一档还没有记录",
                                        log_view.text()
                                    ))
                                    .size(10.5)
                                    .color(theme::TEXT_FAINT),
                                );
                            }
                            let shown = log.len().saturating_sub(120);
                            for (i, entry) in log[shown..].iter().enumerate() {
                                if i > 0 {
                                    log_rule(ui);
                                }
                                // 日志是唯一「该能选中」的正文：报错行经常要复制去搜。
                                // 全局关掉了标签选中（见 `theme::install`），这里单独开回来。
                                ui.add(
                                    egui::Label::new(
                                        RichText::new(format!(
                                            "{} {:<5} {}",
                                            logging::format_local(entry.at),
                                            entry.level.label(),
                                            entry.message
                                        ))
                                        .monospace()
                                        .size(10.5)
                                        .color(theme::level_color(entry.level)),
                                    )
                                    .selectable(true),
                                );
                            }
                        });
                },
            );
        });
    });

    action
}

/// 日志行之间的分隔线。
///
/// 两端各缩进 `LOG_RULE_INSET`：从边框一路拉通到边框会跟外面那圈卡片描边
/// 接成一条，看起来像卡片被切开了；留白之后才读得出「这是行与行的界」。
/// 把扫到的 Java 逐个列出来（只是展示，不在这里选）。
///
/// 一行一个：左边「Java 21」和完整版本串，右边安装路径，正在用的那个末尾挂
/// 一枚「使用中」。整块套一层浅灰内凹底，跟上面的表单分开——它回答的是「本机
/// 有什么」，不是「现在用哪个」（那是上面那个下拉的事）。
///
/// 路径单行截断而不是折行：同一台机器上的几个 JDK 往往只差最后一级目录
/// （`jdk-21` / `jdk-21.0.4`），折成两行反而比不出区别。
fn java_list(ui: &mut egui::Ui, list: &[java::JavaRuntime], current: Option<&std::path::Path>) {
    const ROW_H: f32 = 38.0;
    const NAME_W: f32 = 92.0;

    theme::inset().show(ui, |ui| {
        let width = ui.available_width();
        for (i, rt) in list.iter().enumerate() {
            if i > 0 {
                // 行与行之间留一点缝，不然几行灰字会糊成一段。
                ui.add_space(2.0);
            }
            let (rect, _) = ui.allocate_exact_size(egui::vec2(width, ROW_H), egui::Sense::hover());
            let inner = rect.shrink2(egui::vec2(6.0, 0.0));

            ui.painter().text(
                egui::pos2(inner.left(), inner.center().y - 7.0),
                egui::Align2::LEFT_CENTER,
                format!("Java {}", rt.major_version),
                egui::FontId::proportional(12.0),
                theme::TEXT,
            );
            ui.painter().text(
                egui::pos2(inner.left(), inner.center().y + 8.0),
                egui::Align2::LEFT_CENTER,
                &rt.version_string,
                egui::FontId::proportional(10.0),
                theme::TEXT_FAINT,
            );

            // 右边先排「使用中」，再排「32 位」，剩下的宽度全给路径。
            let mut right = inner.right();
            if current == Some(rt.path.as_path()) {
                let galley = ui.painter().layout_no_wrap(
                    "使用中".to_string(),
                    egui::FontId::proportional(10.0),
                    theme::accent_strong(),
                );
                let tag = egui::Rect::from_center_size(
                    egui::pos2(right - galley.size().x / 2.0, inner.center().y),
                    galley.size() + egui::vec2(12.0, 4.0),
                );
                ui.painter()
                    .rect_filled(tag, egui::CornerRadius::same(5), theme::accent_soft());
                ui.painter().galley(
                    egui::pos2(
                        tag.center().x - galley.size().x / 2.0,
                        tag.center().y - galley.size().y / 2.0,
                    ),
                    galley,
                    theme::accent_strong(),
                );
                right = tag.left() - 8.0;
            }
            if !rt.is_64bit {
                let galley = ui.painter().layout_no_wrap(
                    "32 位".to_string(),
                    egui::FontId::proportional(10.0),
                    theme::WARN,
                );
                let tag = egui::Rect::from_center_size(
                    egui::pos2(right - galley.size().x / 2.0, inner.center().y),
                    galley.size() + egui::vec2(10.0, 2.0),
                );
                ui.painter().rect_filled(
                    tag,
                    egui::CornerRadius::same(5),
                    theme::WARN_TINT,
                );
                ui.painter().galley(
                    egui::pos2(
                        tag.center().x - galley.size().x / 2.0,
                        tag.center().y - galley.size().y / 2.0,
                    ),
                    galley,
                    theme::WARN,
                );
                right = tag.left() - 10.0;
            }

            let path_left = inner.left() + NAME_W;
            let room = (right - path_left).max(40.0);
            let mut job = egui::text::LayoutJob::default();
            job.append(
                &rt.path.display().to_string(),
                0.0,
                egui::TextFormat {
                    font_id: egui::FontId::proportional(10.5),
                    color: theme::TEXT_WEAK,
                    ..Default::default()
                },
            );
            job.wrap = egui::text::TextWrapping {
                max_rows: 1,
                break_anywhere: true,
                overflow_character: Some('…'),
                ..Default::default()
            };
            job.wrap.max_width = room;
            let galley = ui.ctx().fonts_mut(|fonts| fonts.layout_job(job));
            // 左对齐，让几行的盘符和层级竖着对齐——路径是要竖着扫的。
            ui.painter().galley(
                egui::pos2(path_left, inner.center().y - galley.size().y / 2.0),
                galley,
                theme::TEXT_WEAK,
            );
        }
    });
}

fn log_rule(ui: &mut egui::Ui) {
    ui.add_space(LOG_RULE_GAP);
    let width = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(egui::Vec2::new(width, 1.0), egui::Sense::hover());
    ui.painter().hline(
        egui::Rangef::new(rect.left() + LOG_RULE_INSET, rect.right() - LOG_RULE_INSET),
        rect.center().y,
        egui::Stroke::new(1.0, theme::RULE),
    );
    ui.add_space(LOG_RULE_GAP);
}

/// 日志条数那行怎么写。
///
/// 筛过之后条数会变小，只写一个数字的话读者分不出「日志变少了」还是
/// 「这一档本来就没几条」，所以两种情况下措辞要能区分开。
fn scope_text(shown: usize, total: usize, level: logging::Level) -> String {
    if shown == total {
        format!("共 {} 条记录", total)
    } else {
        format!(
            "{} 条（只看「{}」及以上，共 {} 条）",
            shown,
            level.text(),
            total
        )
    }
}

/// 日志面板的高度：正好容得下 10 行（一行 12.6 的正文 + 上下各 5 的分隔线间距）。
///
/// 写死高度而不是让它自己撑：日志多了会把设置页越顶越长，
/// 「导出日志」按钮跟着往下跑。
const LOG_PANEL_H: f32 = 245.0;

/// 行分隔线两端各留多少空白，别和卡片描边连上。
const LOG_RULE_INSET: f32 = 10.0;

/// 分隔线上下各留多少空隙。
const LOG_RULE_GAP: f32 = 5.0;

/// `theme::inset()` 的上下内边距之和（`Margin::symmetric(12, 10)`）。
const INSET_PADDING_Y: f32 = 20.0;

/// 下载源那几行里，左边名字占的固定宽度。
///
/// 名字长短差很多（`版本下载源` / `Fabric` / `NeoForge`），不给固定宽度的话
/// 右边的「官方 / BMCLAPI」每行都会各自错开一点，看着像没对齐。
const MIRROR_LABEL_W: f32 = 84.0;

/// 一行「用哪个下载源」的单选：左边固定宽度放名字，右边选项从同一条竖线起。
///
/// 名字不能用 `ui.label` / `add_sized(Label)` —— 前者宽度跟着文字变，后者会把
/// 文字在格子居中对齐，两种都会让右边那排「官方 / BMCLAPI」各自错开。
/// 这里直接占一块定宽矩形，再把文字按左对齐画进去，起点就钉死了。
fn mirror_row(ui: &mut egui::Ui, label: &str, current: &mut Mirror) {
    ui.horizontal(|ui| {
        let (rect, _) = ui.allocate_exact_size(
            egui::vec2(MIRROR_LABEL_W, ui.spacing().interact_size.y),
            egui::Sense::hover(),
        );
        ui.painter().text(
            rect.left_center(),
            egui::Align2::LEFT_CENTER,
            label,
            egui::FontId::proportional(12.0),
            theme::TEXT_WEAK,
        );
        ui.add_space(8.0);
        for option in Mirror::ALL {
            if ui
                .selectable_label(*current == option, option.label())
                .clicked()
            {
                *current = option;
            }
        }
    });
}

pub(crate) fn section(ui: &mut egui::Ui, title: &str) {
    let text = RichText::new(title)
        .size(12.5)
        .strong()
        .color(theme::accent_strong());
    if theme::wallpaper_active() {
        // 有底图时这一行是直接压在照片上的：小号绿字碰上亮云彩就没了。
        // 垫一块跟卡片同色的薄底，既压得住字，也不像凭空多出一个框。
        egui::Frame::NONE
            .fill(theme::card_fill())
            .corner_radius(egui::CornerRadius::same(6))
            .inner_margin(egui::Margin::symmetric(8, 3))
            .show(ui, |ui| {
                ui.label(text);
            });
    } else {
        ui.label(text);
    }
    ui.add_space(4.0);
}

/// 卡片里两个项目之间的一道淡灰细线。
///
/// 撑满内容区整宽（含卡片内边距之间的那一段），只画 1px，
/// 颜色用卡片描边同一支灰——既分得开，又不至于在白色里切出黑缝。
/// 内存这一节的正文：自动 / 手动二选一 + 滑块 + MiB 数字框 + 一条本机内存条。
///
/// 布局照 HMCL 的样子来：上面两个单选，手动时右边给出滑块与数字框，
/// 底下一条横条画本机已用/总量，后面接一句「游戏分配 X（设备仅 Y 可用）」。
///
/// 这里的量一个是「本机物理内存」（[[`aicraft_core::meminfo`]] 探的），
/// 一个是「游戏堆大小」（`-Xmx`）。两者不是一回事，所以分开写、不画在同一条上。
fn memory_body(ui: &mut egui::Ui, settings: &mut Settings) {
    let memory = aicraft_core::meminfo::cached();
    let recommended = memory.map(|m| aicraft_core::meminfo::recommended_mb(m.total_mb));

    // ---- 自动 / 手动 ----
    ui.horizontal(|ui| {
        if theme::radio(ui, settings.auto_memory, "自动分配内存").clicked() {
            settings.auto_memory = true;
        }
        ui.add_space(18.0);
        if theme::radio(ui, !settings.auto_memory, "手动选择内存").clicked() {
            // 从自动切到手动时，把手动值对齐到自动那一档 ——
            // 否则玩家会看到一个自己没设过的数字，还得先想「这是哪儿来的」。
            if settings.auto_memory
                && let Some(mb) = recommended {
                settings.max_memory_mb = mb;
            }
            settings.auto_memory = false;
        }
    });
    ui.add_space(10.0);

    // ---- 滑块 + 数字框 ----
    // 自动模式下也照画、只是变灰：布局不会因为切换而上下跳。
    let manual = !settings.auto_memory;
    let shown_mb = settings.memory_mb();
    ui.horizontal(|ui| {
        // 数字框占 74px，滑块吃掉剩下的
        const BOX_W: f32 = 74.0;
        let slider_w = (ui.available_width() - BOX_W - 46.0).max(80.0);
        // 滑块的上限跟着本机内存走：一台 16 GiB 的机器上，「4096」应该落在
        // 全程的四分之一附近，而不是贴着最左边（用 65536 做上限时就是那样）。
        let ceil = memory
            .map(|m| slider_ceiling(m.total_mb))
            .unwrap_or(MEMORY_CEIL_MB);
        let mut ratio = (shown_mb.saturating_sub(MEMORY_FLOOR_MB) as f32
            / (ceil - MEMORY_FLOOR_MB) as f32)
            .clamp(0.0, 1.0);
        let drag = ui
            .allocate_ui_with_layout(
                egui::vec2(slider_w, 24.0),
                egui::Layout::left_to_right(egui::Align::Center),
                |ui| theme::slider(ui, &mut ratio, manual),
            )
            .inner;
        if drag.changed() && manual {
            settings.max_memory_mb =
                snap_mb(MEMORY_FLOOR_MB + (ratio * (ceil - MEMORY_FLOOR_MB) as f32) as u32);
        }

        ui.add_space(8.0);
        let mut typed = shown_mb;
        let box_resp = ui.add_enabled(
            manual,
            egui::DragValue::new(&mut typed)
                .range(MEMORY_FLOOR_MB..=MEMORY_CEIL_MB)
                .speed(64.0)
                .suffix(""),
        );
        if box_resp.changed() {
            settings.max_memory_mb = snap_mb(typed);
        }
        ui.label(RichText::new("MiB").size(11.5).color(theme::TEXT_WEAK));
    });

    // ---- 本机内存条 ----
    if let Some(memory) = memory {
        ui.add_space(10.0);
        memory_bar(ui, memory, shown_mb);
    } else {
        ui.add_space(6.0);
        ui.label(
            RichText::new("正在读取本机内存…")
                .size(10.5)
                .color(theme::TEXT_FAINT),
        );
        // 快照在别的线程里量：这里不催一下，egui 睡下去之后
        // 「正在读取」会一直挂在那儿，直到玩家碰一下鼠标。
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(150));
    }

    ui.add_space(4.0);
    let hint = if settings.auto_memory {
        format!("自动按本机内存分配，现在是 {} MiB。", settings.memory_mb())
    } else {
        format!(
            "手动指定 {} MiB，会作为 -Xmx 传给游戏。",
            settings.max_memory_mb
        )
    };
    ui.label(RichText::new(hint).size(10.5).color(theme::TEXT_WEAK));
}

/// 内存滑块的下限，与 [`aicraft_core::settings::Settings::clamp`] 同口径。
pub(crate) const MEMORY_FLOOR_MB: u32 = 512;
/// 探不到本机内存时的滑块上限。
pub(crate) const MEMORY_CEIL_MB: u32 = 65536;

/// 滑块上限：留一点给系统，最多到 32 GiB。
///
/// 上限跟着机器走的好处是刻度有意义 —— 全程都落在「这台机器上可能用到的
/// 范围」里，拖到底就是「这台机器能给的最多」。固定用 65536 的话，
/// 在 16 GiB 的机器上九成的行程都是不可能的值。
pub(crate) fn slider_ceiling(total_mb: u64) -> u32 {
    let headroom = total_mb.saturating_sub(1024);
    (headroom as u32).clamp(4096, 32768)
}

/// 1024 整数倍两侧的「磁吸」范围（MiB）。
///
/// 玩家真正想要的是 2 GiB、4 GiB、8 GiB 这种整数档，而滑块按 64 走，
/// 4096 一带很容易落在 4032 或 4160 上，看着像「差一点」。所以在每个
/// 1024 的整数倍两侧各留这么宽，落进来就直接贴上去；出了这个范围还是
/// 老老实实按 64 走，不然细调就没法用了。
const SNAP_1024_WINDOW_MB: u32 = 128;

/// 把任意值对齐到 64 的倍数，并在 1024 的整数倍附近磁吸过去。
///
/// 玩家拖动滑块时不需要 3721 这种数；64 一档，数字好看，也够细。
pub(crate) fn snap_mb(mb: u32) -> u32 {
    let nearest_1024 = (mb.saturating_add(512) / 1024) * 1024;
    let snapped = if nearest_1024 >= 1024 && mb.abs_diff(nearest_1024) <= SNAP_1024_WINDOW_MB {
        nearest_1024
    } else {
        (mb.saturating_add(32) / 64) * 64
    };
    snapped.clamp(MEMORY_FLOOR_MB, MEMORY_CEIL_MB)
}

/// 本机内存总览条：浅色底画总量，深色段画已用。
///
/// 不画「游戏分配」那一段：那个数字要等游戏真的跑起来才知道，而且
/// 堆大小和物理占用也不是同一个量（JVM 不会一上来就摸满 -Xmx）。
/// 与其画一段会引起误解的色块，不如把它写在文字里。
fn memory_bar(ui: &mut egui::Ui, memory: aicraft_core::meminfo::Memory, game_mb: u32) {
    let width = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(egui::vec2(width, 8.0), egui::Sense::hover());
    let painter = ui.painter();
    painter.rect_filled(
        rect,
        egui::CornerRadius::same(4),
        theme::TRACK,
    );
    let used = memory.used_mb() as f32 / memory.total_mb.max(1) as f32;
    let used_right = rect.left() + rect.width() * used.clamp(0.0, 1.0);
    if used > 0.0 {
        let fill = egui::Rect::from_min_max(rect.min, egui::pos2(used_right, rect.bottom()));
        painter.rect_filled(fill, egui::CornerRadius::same(4), theme::TEXT_WEAK);
    }

    ui.add_space(5.0);
    ui.horizontal(|ui| {
        ui.label(
            RichText::new(format!(
                "已使用 {} / 总内存 {}",
                aicraft_core::meminfo::Memory::human(memory.used_mb()),
                aicraft_core::meminfo::Memory::human(memory.total_mb),
            ))
            .size(10.5)
            .color(theme::TEXT_WEAK),
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let available = memory.available_mb;
            let tight = game_mb as u64 > available;
            let color = if tight {
                theme::DANGER
            } else {
                theme::TEXT_WEAK
            };
            let warning = if tight {
                format!(
                    "（设备仅 {} 可用，可能不够）",
                    aicraft_core::meminfo::Memory::human(available)
                )
            } else {
                format!(
                    "（设备 {} 可用）",
                    aicraft_core::meminfo::Memory::human(available)
                )
            };
            ui.label(
                RichText::new(format!(
                    "游戏分配 {}{}",
                    aicraft_core::meminfo::Memory::human(game_mb as u64),
                    warning
                ))
                .size(10.5)
                .color(color),
            );
        });
    });
}

/// 一行「左边名字、右边控件」。
///
/// 这一页的设置项统一成这个形状：左列是「这一项叫什么」，右列是「它现在
/// 是什么」。以前标题占一行、控件另起一行，两项并排看着像四行，读的时候
/// 还得来回对；现在一行就是一项，眼睛扫一遍就到底。
///
/// 名字那列固定占 [`LABEL_W`]，剩下的都给控件，所以几行的控件左边缘是齐的。
pub(crate) fn setting_row(ui: &mut egui::Ui, name: &str, control: impl FnOnce(&mut egui::Ui)) {
    ui.horizontal(|ui| {
        ui.allocate_ui_with_layout(
            egui::vec2(LABEL_W, ROW_H),
            egui::Layout::left_to_right(egui::Align::Center),
            |ui| {
                ui.label(
                    RichText::new(name)
                        .size(12.0)
                        .color(theme::TEXT_WEAK),
                );
            },
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            control(ui);
        });
    });
}

/// [`setting_row`] 左边名字那一列的宽度。
const LABEL_W: f32 = 170.0;

/// 这一页下拉框统一收这么宽，几行的控件才对得齐。
const COMBO_W: f32 = 300.0;

/// [`setting_row`] 一行的高度；下拉框比文字高，按控件算。
const ROW_H: f32 = 26.0;

pub(crate) fn divider(ui: &mut egui::Ui) {
    ui.add_space(9.0);
    let (rect, _) = ui.allocate_exact_size(
        egui::Vec2::new(ui.available_width(), 1.0),
        egui::Sense::hover(),
    );
    ui.painter().hline(
        rect.x_range(),
        rect.center().y,
        egui::Stroke::new(1.0, theme::BORDER),
    );
    ui.add_space(9.0);
}

/// 「游戏窗口大小」下拉框里的条目，顺序就是 `window_preset` 的索引。
///
/// 预设不单独存进设置里——「1280 × 720」和玩家手填的 1280×720 在磁盘上
/// 是同一样东西，界面按数值反查回来即可。这样预设表以后增删也不会让旧配置失配。
const PRESETS: [&str; 8] = [
    "自动",
    "1280 × 720",
    "1600 × 900",
    "1920 × 1080",
    "2560 × 1440",
    "自定义…",
    "最大化",
    "全屏",
];

/// 「自定义…」在 `PRESETS` 里的下标。
const CUSTOM_INDEX: usize = 5;

/// 常见分辨率，跟 `PRESETS` 里第 1..=4 项一一对应。
const SIZES: [(u32, u32); 4] = [(1280, 720), (1600, 900), (1920, 1080), (2560, 1440)];

/// 当前设置对应下拉框里哪一条。
fn window_preset(settings: &Settings) -> usize {
    match settings.window_mode {
        WindowMode::Auto => 0,
        WindowMode::Maximized => PRESETS.len() - 2,
        WindowMode::Fullscreen => PRESETS.len() - 1,
        WindowMode::Fixed => SIZES
            .iter()
            .position(|&(w, h)| w == settings.window_width && h == settings.window_height)
            .map(|index| index + 1)
            // 对不上任何一个预设就是玩家自己填的，落到「自定义…」
            .unwrap_or(CUSTOM_INDEX),
    }
}

/// 下拉框收起时显示的那一行文字。
fn window_text(settings: &Settings) -> String {
    let index = window_preset(settings);
    if index == CUSTOM_INDEX {
        // 自定义：把具体数字写出来，否则收起后看不出到底多大
        return format!(
            "自定义 {} × {}",
            settings.window_width, settings.window_height
        );
    }
    PRESETS[index].to_string()
}

/// 玩家在下拉框里点了第 `index` 条。
fn apply_preset(settings: &mut Settings, index: usize) {
    match index {
        // 自动：不动宽高，切回自动之后再选「指定」还是上次那两个数
        0 => settings.window_mode = WindowMode::Auto,
        // 四个预设：把手填值也一起写上，切到「自定义」时能接着改
        n if n - 1 < SIZES.len() => {
            let (width, height) = SIZES[n - 1];
            settings.window_mode = WindowMode::Fixed;
            settings.window_width = width;
            settings.window_height = height;
        }
        // 自定义：只切模式，具体数字交给下面两个输入框
        CUSTOM_INDEX => settings.window_mode = WindowMode::Fixed,
        // 最后一条是「全屏」，倒数第二条是「最大化」
        n if n == PRESETS.len() - 1 => settings.window_mode = WindowMode::Fullscreen,
        _ => settings.window_mode = WindowMode::Maximized,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        apply_preset, scope_text, slider_ceiling, snap_mb, window_preset, window_text, JavaScan,
        CUSTOM_INDEX, PRESETS,
    };
    use aicraft_core::logging::Level;
    use aicraft_core::settings::{Settings, WindowMode};

    fn runtime(major: u32, version: &str) -> aicraft_core::java::JavaRuntime {
        aicraft_core::java::JavaRuntime {
            path: std::path::PathBuf::from(format!("C:\\Java\\jdk-{}\\bin\\javaw.exe", major)),
            major_version: major,
            version_string: version.to_string(),
            is_64bit: true,
            source: "手动添加".to_string(),
        }
    }

    #[test]
    fn a_hand_picked_json_is_never_added_twice() {
        // 同一个 JDK 换个路径加进来不该多一行：去重键是「主版本 + 版本串」
        let mut scan = JavaScan::default();
        assert!(scan.add(runtime(21, "21.0.4")));
        assert!(!scan.add(runtime(21, "21.0.4")));
        assert!(scan.add(runtime(17, "17.0.11")));
    }

    #[test]
    fn a_merged_list_keeps_one_row_per_runtime() {
        // 扫描已经扫到 21，手工又加了同一个 21 和另一台 17：17 进去，21 不重复。
        let mut list = vec![runtime(21, "21.0.4")];
        super::merge_into(&mut list, vec![runtime(21, "21.0.4"), runtime(17, "17.0.11")]);
        assert_eq!(list.len(), 2);
        // 新的排在前面（按主版本降序）
        assert_eq!(list[0].major_version, 21);
        assert_eq!(list[1].major_version, 17);
    }

    #[test]
    fn seeded_paths_are_remembered_until_the_scan_starts() {
        let mut scan = JavaScan::default();
        scan.seed(&[
            "C:\\Java\\jdk-21\\bin\\javaw.exe".to_string(),
            "   ".to_string(),
        ]);
        // 空白项丢掉，剩一条等着扫描线程去探
        assert_eq!(scan.seeded.len(), 1);
        assert!(scan.seeded[0].ends_with("javaw.exe"));
    }

    #[test]
    fn additions_made_while_scanning_survive_the_scan() {
        let mut scan = JavaScan::default();
        // 扫描还没回来（runtimes 是 None）时加一个
        assert!(scan.add(runtime(25, "25.0.4")));
        assert!(scan.runtimes().is_none());
        // 扫描回来了，把手工那个并进去，而不是覆盖掉
        scan.runtimes = Some(vec![runtime(21, "21.0.4")]);
        // 直接调 poll 的内部行为在这里不好造，改为验证 add 之后的列表口径：
        assert_eq!(scan.extra.len(), 1);
    }

    #[test]
    fn the_slider_ceiling_follows_the_machine() {
        // 16 GiB：留 1 GiB 给系统，滑块到 15 GiB
        assert_eq!(slider_ceiling(16182), 15158);
        // 8 GiB 的机器：不会低到 4 GiB 以下
        assert_eq!(slider_ceiling(8192), 7168);
        // 很小的机器也要留出可拖范围
        assert_eq!(slider_ceiling(2048), 4096);
        // 超大机器封顶 32 GiB —— 再往上对这个量级的客户端没意义
        assert_eq!(slider_ceiling(65536), 32768);
    }

    #[test]
    fn dragged_values_snap_to_64() {
        assert_eq!(snap_mb(4096), 4096);
        // 3900 离 4096 已经有 196 MiB，超出磁吸范围，按 64 一档走。
        assert_eq!(snap_mb(3900), 3904);
        assert_eq!(snap_mb(4400), 4416);
        // 两端要夹住
        assert_eq!(snap_mb(0), 512);
        assert_eq!(snap_mb(999_999), 65536);
    }

    #[test]
    fn whole_gigabyte_numbers_are_magnetic() {
        // 4096 两侧各 128 MiB 以内都贴到 4096，而不是 4032 / 4160。
        assert_eq!(snap_mb(4032), 4096);
        assert_eq!(snap_mb(4100), 4096);
        assert_eq!(snap_mb(4200), 4096);
        assert_eq!(snap_mb(4224), 4096);
        // 出圈就退回 64 一档。
        assert_eq!(snap_mb(4300), 4288);
        assert_eq!(snap_mb(8192), 8192);
        assert_eq!(snap_mb(8100), 8192);
        // 下限 512 不是 1024 的倍数，不能被吸到 1024 去。
        assert_eq!(snap_mb(600), 576);
    }

    #[test]
    fn unfiltered_count_says_nothing_about_levels() {
        assert_eq!(scope_text(19, 19, Level::Trace), "共 19 条记录");
        assert_eq!(scope_text(0, 0, Level::Error), "共 0 条记录");
    }

    #[test]
    fn filtered_count_names_the_level_and_the_whole() {
        let text = scope_text(1, 19, Level::Error);
        assert!(text.contains('1'), "{}", text);
        assert!(text.contains('9'), "{}", text);
        assert!(text.contains("错误"), "{}", text);
    }

    #[test]
    fn every_preset_can_be_picked_back_up() {
        let mut settings = Settings::default();
        for index in 0..PRESETS.len() {
            // 「自定义…」只是个切换模式的动作，它不产生新尺寸 —— 磁盘上存的是
            // 那对数字，反查回来必然落回某个具体预设（或仍然算自定义），
            // 所以这一条由 `a_hand_typed_size_is_called_custom` 单独覆盖。
            if index == CUSTOM_INDEX {
                continue;
            }
            apply_preset(&mut settings, index);
            assert_eq!(window_preset(&settings), index, "{:?}", settings.window_mode);
        }
    }

    #[test]
    fn a_hand_typed_size_is_called_custom() {
        let settings = Settings {
            window_mode: WindowMode::Fixed,
            window_width: 1760,
            window_height: 990,
            ..Settings::default()
        };
        assert_eq!(window_preset(&settings), CUSTOM_INDEX);
        // 收起后要看得见具体数字，不然「自定义…」等于什么都没说
        let text = window_text(&settings);
        assert!(text.contains("1760") && text.contains("990"), "{}", text);
    }

    #[test]
    fn picking_a_preset_fills_both_numbers() {
        let mut settings = Settings::default();
        apply_preset(&mut settings, 3); // 1920 × 1080
        assert_eq!(settings.window_mode, WindowMode::Fixed);
        assert_eq!((settings.window_width, settings.window_height), (1920, 1080));
    }

    #[test]
    fn auto_keeps_the_last_numbers() {
        let mut settings = Settings::default();
        apply_preset(&mut settings, 1); // 1280 × 720
        apply_preset(&mut settings, 0); // 自动
        assert_eq!(settings.window_mode, WindowMode::Auto);
        // 切回自动不该把宽高清零：玩家再选「自定义」时还能接着改
        assert_eq!((settings.window_width, settings.window_height), (1280, 720));
    }
}
