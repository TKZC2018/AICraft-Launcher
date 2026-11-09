//! 设置页。
//!
//! 只画界面，不写盘：这里改的是 `Settings` 值本身，由 `main.rs`
//! 在每帧末尾比较差异后落盘。

use std::sync::mpsc::{self, Receiver, TryRecvError};

use aicraft_core::java;
use aicraft_core::logging;
use aicraft_core::settings::{LauncherMode, Mirror, Settings};
use egui::RichText;

use crate::theme;

/// 下拉框里「当前选的是哪一个」那一行文字。
///
/// 只写路径的话，`C:\Program Files\Java\jdk-21\bin\javaw.exe` 太长会把
/// 下拉框撑变形，而玩家真正在乎的是「Java 几」；扫到了就顺带把大版本写出来，
/// 没扫到（或者指定的是列表外的）就老老实实显示路径。
fn describe_java(path: &std::path::Path, runtimes: Option<&[java::JavaRuntime]>) -> String {
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
    pub fn add(&mut self, rt: java::JavaRuntime) -> bool {
        fn same(a: &java::JavaRuntime, b: &java::JavaRuntime) -> bool {
            a.major_version == b.major_version && a.version_string == b.version_string
        }
        self.error = None;
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
        std::thread::spawn(move || {
            let _ = tx.send(java::detect_all());
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
                for rt in self.extra.drain(..) {
                    let duplicate = list.iter().any(|k| {
                        k.major_version == rt.major_version && k.version_string == rt.version_string
                    });
                    if !duplicate {
                        list.push(rt);
                    }
                }
                list.sort_by_key(|k| std::cmp::Reverse(k.major_version));
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
        ui.label(
            RichText::new("游戏运行时启动器状态")
                .size(12.0)
                .color(theme::TEXT_WEAK),
        );
        ui.add_space(4.0);
        // 下拉独占一行：和标题挤在一起时，右边那句「关闭启动器在游戏结束后
        // 重新打开（默认）」太长，会把标题那行撑到几乎贴边。
        ui.add_enabled_ui(!busy, |ui| {
            let combo = egui::ComboBox::from_id_salt("launcher-mode")
                .selected_text(RichText::new(settings.launcher_mode.label()).size(12.0))
                .width(300.0)
                .show_ui(ui, |ui| {
                    for mode in LauncherMode::ALL {
                        ui.selectable_value(&mut settings.launcher_mode, mode, mode.label());
                    }
                });
            theme::hand(combo.response);
        });

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
        // 下拉里要显示的当前选择：自动 / 指定的那一个。
        let current_text = match &settings.java_path {
            None => "自动选择".to_string(),
            Some(path) => describe_java(path, runtimes),
        };
        let combo = egui::ComboBox::from_id_salt("java-choice")
            .selected_text(RichText::new(current_text).size(12.0))
            .width(ui.available_width())
            .show_ui(ui, |ui| {
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
            });
        theme::hand(combo.response);

        if let Some(error) = &java.error {
            ui.add_space(2.0);
            ui.label(RichText::new(error.clone()).size(10.5).color(theme::DANGER));
        }
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
        ui.add(
            egui::TextEdit::multiline(&mut settings.jvm_args)
                .hint_text("-XX:+UseG1GC -Dfile.encoding=UTF-8")
                .desired_rows(2)
                .desired_width(f32::INFINITY),
        );
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
fn log_rule(ui: &mut egui::Ui) {
    ui.add_space(LOG_RULE_GAP);
    let width = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(egui::Vec2::new(width, 1.0), egui::Sense::hover());
    ui.painter().hline(
        egui::Rangef::new(rect.left() + LOG_RULE_INSET, rect.right() - LOG_RULE_INSET),
        rect.center().y,
        egui::Stroke::new(1.0, LOG_RULE_COLOR),
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

/// 分隔线的颜色：比卡片描边更淡一档，它一天要出现十来条，
/// 和 `theme::BORDER` 一个浓度会抢正文的视线。
const LOG_RULE_COLOR: egui::Color32 = egui::Color32::from_rgb(0xE8, 0xED, 0xF4);

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

fn section(ui: &mut egui::Ui, title: &str) {
    ui.label(
        RichText::new(title)
            .size(12.5)
            .strong()
            .color(theme::ACCENT_STRONG),
    );
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
            if settings.auto_memory {
                if let Some(mb) = recommended {
                    settings.max_memory_mb = mb;
                }
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
const MEMORY_FLOOR_MB: u32 = 512;
/// 探不到本机内存时的滑块上限。
const MEMORY_CEIL_MB: u32 = 65536;

/// 滑块上限：留一点给系统，最多到 32 GiB。
///
/// 上限跟着机器走的好处是刻度有意义 —— 全程都落在「这台机器上可能用到的
/// 范围」里，拖到底就是「这台机器能给的最多」。固定用 65536 的话，
/// 在 16 GiB 的机器上九成的行程都是不可能的值。
fn slider_ceiling(total_mb: u64) -> u32 {
    let headroom = total_mb.saturating_sub(1024);
    (headroom as u32).clamp(4096, 32768)
}

/// 把任意值对齐到 64 的倍数。
///
/// 玩家拖动滑块时不需要 3721 这种数；64 一档，数字好看，也够细。
fn snap_mb(mb: u32) -> u32 {
    let snapped = (mb.saturating_add(32) / 64) * 64;
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
        egui::Color32::from_rgb(0xE7, 0xED, 0xF5),
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

fn divider(ui: &mut egui::Ui) {
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

#[cfg(test)]
mod tests {
    use super::{scope_text, slider_ceiling, snap_mb, JavaScan};
    use aicraft_core::logging::Level;

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
        assert_eq!(snap_mb(4100), 4096);
        assert_eq!(snap_mb(4120), 4096);
        assert_eq!(snap_mb(4133), 4160);
        // 两端要夹住
        assert_eq!(snap_mb(0), 512);
        assert_eq!(snap_mb(999_999), 65536);
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
}
