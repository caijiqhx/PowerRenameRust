//! PowerRename — egui GUI 主程序。
//!
//! 布局：
//!   ┌────────────────────────────────────────────┐
//!   │ 路径输入 [加载] 递归□ 深度[ ] 含文件□ 含文件夹□ │
//!   │ 名称包含[ ] 排除[ ] 正则□                   │
//!   ├──────────────┬─────────────────────────────┤
//!   │ +添加规则 应用撤销 │                          │
//!   │ [删除][↑][↓][清空]│                          │
//!   │ 规则列表        │ 预览树（原名 → 新名 + 状态） │
//!   │ 规则表单        │                             │
//!   ├──────────────┴─────────────────────────────┤
//!   │ 状态栏                                       │
//!   └────────────────────────────────────────────┘

// 发布版不挂控制台窗口（避免运行时出现黑色终端）；debug 构建保留便于看输出
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::path::{Path, PathBuf};

use eframe::egui;

use power_rename::apply::{apply_renames, UndoManager};
use power_rename::fs_tree::{load_tree, flatten_tree, LoadOptions, TreeNode};
use power_rename::preview::{compute_preview, PreviewStatus};

/// 规则表单数据（GUI 编辑用，提交时转成引擎 Rule）
#[derive(Debug, Clone, PartialEq)]
enum RuleForm {
    Replace {
        search: String,
        replace: String,
        case_sensitive: bool,
        scope: usize, // 0=完整名 1=主名 2=扩展名
    },
    Regex {
        pattern: String,
        replace: String,
        scope: usize,
    },
    Case {
        mode: usize, // 0=lower 1=upper 2=title 3=capitalize
        scope: usize,
    },
    Prefix {
        text: String,
    },
    Suffix {
        text: String,
    },
    Number {
        pos: usize, // 0=前缀 1=后缀
        start: String,
        step: String,
        digits: String,
        sep: String,
    },
    Ext {
        text: String,
    },
    Strip {
        chars: String,
        scope: usize,
    },
    Trim {
        underscore: bool,
    },
    List {
        mapping: std::collections::HashMap<String, String>,
    },
}

impl RuleForm {
    fn summary(&self) -> String {
        match self {
            RuleForm::Replace { search, replace, case_sensitive, .. } => {
                let cs = if *case_sensitive { "敏感" } else { "忽略大小写" };
                format!("替换 [{search}] → [{replace}] ({cs})")
            }
            RuleForm::Regex { pattern, replace, .. } => {
                format!("正则 [{pattern}] → [{replace}]")
            }
            RuleForm::Case { mode, .. } => {
                let names = ["小写", "大写", "Title", "Capitalize"];
                let m = names.get(*mode).copied().unwrap_or("小写");
                format!("大小写转换（{m}）")
            }
            RuleForm::Prefix { text } => format!("添加前缀 [{text}]"),
            RuleForm::Suffix { text } => format!("添加后缀 [{text}]"),
            RuleForm::Number { pos, start, step, digits, sep } => {
                let p = if *pos == 0 { "前缀" } else { "后缀" };
                format!("序列编号（{p} 从 {start} 步 {step} {digits}位 分隔[{sep}]）")
            }
            RuleForm::Ext { text } => format!("替换扩展名 [{text}]"),
            RuleForm::Strip { chars, .. } => format!("移除字符 [{chars}]"),
            RuleForm::Trim { underscore } => {
                if *underscore { "压缩空白（换下划线）".to_string() } else { "压缩空白".to_string() }
            }
            RuleForm::List { mapping } => format!("清单 [{} 条映射]", mapping.len()),
        }
    }

    fn to_rule(&self) -> power_rename::rules::Rule {
        let scope = match self.scope() {
            1 => power_rename::rules::Scope::Stem,
            2 => power_rename::rules::Scope::Ext,
            _ => power_rename::rules::Scope::Full,
        };
        let case_mode = |n: usize| match n {
            1 => power_rename::rules::CaseMode::Upper,
            2 => power_rename::rules::CaseMode::Title,
            3 => power_rename::rules::CaseMode::Capitalize,
            _ => power_rename::rules::CaseMode::Lower,
        };
        match self {
            RuleForm::Replace { search, replace, case_sensitive, .. } => {
                power_rename::rules::Rule::Replace {
                    search: search.clone(),
                    replace: replace.clone(),
                    case_sensitive: *case_sensitive,
                    scope,
                }
            }
            RuleForm::Regex { pattern, replace, .. } => power_rename::rules::Rule::Regex {
                pattern: pattern.clone(),
                replace: replace.clone(),
                scope,
            },
            RuleForm::Case { mode, .. } => power_rename::rules::Rule::Case {
                mode: case_mode(*mode),
                scope,
            },
            RuleForm::Prefix { text } => power_rename::rules::Rule::Prefix { text: text.clone() },
            RuleForm::Suffix { text } => power_rename::rules::Rule::Suffix { text: text.clone() },
            RuleForm::Number { pos, start, step, digits, sep } => {
                power_rename::rules::Rule::Number {
                    pos: match pos {
                        0 => power_rename::rules::NumberPos::Prefix,
                        _ => power_rename::rules::NumberPos::Suffix,
                    },
                    start: start.trim().parse().unwrap_or(1),
                    step: step.trim().parse().unwrap_or(1),
                    digits: digits.trim().parse().unwrap_or(2),
                    sep: sep.clone(),
                }
            }
            RuleForm::Ext { text } => power_rename::rules::Rule::Ext { text: text.clone() },
            RuleForm::Strip { chars, .. } => power_rename::rules::Rule::Strip {
                chars: chars.clone(),
                scope,
            },
            RuleForm::Trim { underscore } => power_rename::rules::Rule::Trim { underscore: *underscore },
            RuleForm::List { mapping } => power_rename::rules::Rule::List {
                mapping: mapping.clone(),
            },
        }
    }

    fn scope(&self) -> usize {
        match self {
            RuleForm::Replace { scope, .. }
            | RuleForm::Regex { scope, .. }
            | RuleForm::Case { scope, .. }
            | RuleForm::Strip { scope, .. } => *scope,
            _ => 0,
        }
    }

    fn set_scope(&mut self, s: usize) {
        match self {
            RuleForm::Replace { scope, .. }
            | RuleForm::Regex { scope, .. }
            | RuleForm::Case { scope, .. }
            | RuleForm::Strip { scope, .. } => *scope = s,
            _ => {}
        }
    }
}

/// 预览信息（GUI 渲染用）
#[derive(Debug, Clone)]
struct PreviewRow {
    new_name: String,
    status: PreviewStatus,
    note: String,
}

/// 预览区待执行动作（渲染时收集，面板结束后统一执行）。
#[derive(Debug)]
enum PreviewAction {
    None,
    /// 双击目录名：打开文件夹
    Open(std::path::PathBuf),
    /// 双击文件名：打开所在文件夹并定位文件
    Reveal(std::path::PathBuf),
    /// 表头右键：刷新预览
    Refresh,
}

struct RenameApp {
    dir_input: String,
    recursive: bool,
    include_files: bool,
    include_dirs: bool,
    inc_text: String,
    exc_text: String,
    use_regex: bool,

    rules: Vec<RuleForm>,
    selected_rule: Option<usize>,

    tree: Option<TreeNode>,
    /// path → 预览信息（可改名节点的预览结果）
    preview_by_path: std::collections::HashMap<std::path::PathBuf, PreviewRow>,
    /// 预览树中「已展开」的目录路径
    expanded: std::collections::HashSet<std::path::PathBuf>,
    /// 预览中当前点击选中的行（path；None = 未选中）
    selected_preview: Option<std::path::PathBuf>,
    /// 预览区缩放倍率（Ctrl+滚轮，0.5~2.0，仅作用于预览表格）
    preview_zoom: f32,
    /// 预览区状态筛选（None = 全部显示；Some(status) = 只显示该状态的节点）
    preview_filter: Option<PreviewStatus>,
    /// 清单映射查看窗口（存规则序号，None=关闭）
    mapping_view: Option<usize>,
    /// 预览统计信息（共 x 节点 | 可改名 x ...），显示在顶部面板
    preview_stats: String,
    status_msg: String,
    undo: UndoManager,
    /// 截图钩子（仅供验收）：PR_CAPTURE 指向输出路径时，启动后自截图一帧 BMP 并退出
    capture_path: Option<PathBuf>,
    capture_sent: bool,
    frame_count: u32,
}

impl RenameApp {
    fn new() -> Self {
        // 截图钩子：PR_CAPTURE=<路径> 时启动后自截图一帧 BMP 并退出（仅供验收，不影响正常使用）
        let capture_path = std::env::var_os("PR_CAPTURE")
            .filter(|s| !s.is_empty())
            .map(PathBuf::from);
        // 演示规则（仅供验收截图）：PR_DEMO=1 时注入一组规则，让预览呈现
        // 正常改名/冲突/无变化三种状态。可在 PR_CAPTURE 截图前设好路径，
        // 规则启动即就位，等待期间即可看到带新名称和状态高亮的完整预览。
        let mut rules: Vec<RuleForm> = Vec::new();
        if std::env::var_os("PR_DEMO").is_some() {
            rules = Self::demo_rules();
        }
        Self {
            dir_input: String::new(),
            recursive: true,
            include_files: true,
            include_dirs: false,
            inc_text: String::new(),
            exc_text: String::new(),
            use_regex: false,
            rules,
            selected_rule: None,
            tree: None,
            preview_by_path: std::collections::HashMap::new(),
            expanded: std::collections::HashSet::new(),
            selected_preview: None,
            preview_zoom: 1.0,
            preview_filter: None,
            mapping_view: None,
            preview_stats: String::new(),
            status_msg: String::new(),
            undo: UndoManager::new(),
            capture_path,
            capture_sent: false,
            frame_count: 0,
        }
    }

    fn load_options(&self) -> LoadOptions {
        LoadOptions {
            recursive: self.recursive,
            include_files: self.include_files,
            include_dirs: self.include_dirs,
            inc_text: self.inc_text.trim().to_string(),
            exc_text: self.exc_text.trim().to_string(),
            use_regex: self.use_regex,
        }
    }

    fn reload(&mut self) {
        self.tree = None;
        let path = PathBuf::from(self.dir_input.trim());
        if !path.is_dir() {
            self.status_msg = format!("目录不存在：{}", path.display());
            return;
        }
        let opts = self.load_options();
        // 深度限制：LoadOptions 无字段，非递归时由 recursive=false 控制；深度留待扩展
        let tree = load_tree(&path, &opts);
        // 预览行构建：展平树 + 冲突检测（可改名节点参与改名）
        let mut nodes = Vec::new();
        flatten_tree(&tree, &mut nodes);
        // 参与改名的节点 → entries（带原路径）
        let mut entries: Vec<power_rename::fs_tree::FileEntry> = Vec::new();
        for n in &nodes {
            if n.renameable {
                entries.push(power_rename::fs_tree::FileEntry {
                    path: n.path.clone(),
                    name: n.name.clone(),
                    is_dir: n.is_dir,
                });
            }
        }
        let rules: Vec<power_rename::rules::Rule> = self.rules.iter().map(|r| r.to_rule()).collect();
        let items = compute_preview(&entries, &rules);
        // 用「完整路径」做 key 查预览（文件名跨目录可能重复，路径唯一）
        let by_path: std::collections::HashMap<&Path, &power_rename::preview::PreviewItem> =
            items.iter().map(|i| (i.entry.path.as_path(), i)).collect();

        // 构建 path→预览映射（仅可改名节点）
        self.preview_by_path.clear();
        let mut stack: Vec<&TreeNode> = Vec::new();
        stack.push(&tree);
        while let Some(node) = stack.pop() {
            if let Some(p) = by_path.get(node.path.as_path()).copied() {
                self.preview_by_path.insert(
                    node.path.clone(),
                    PreviewRow {
                        new_name: p.new_name.clone(),
                        status: p.status,
                        note: p.note.clone(),
                    },
                );
            }
            for c in node.children.iter().rev() {
                stack.push(c);
            }
        }
        self.tree = Some(tree);
        let ok = self.preview_by_path.values().filter(|r| r.status == PreviewStatus::Ok).count();
        let conflict = self.preview_by_path.values().filter(|r| r.status == PreviewStatus::Conflict).count();
        let error = self.preview_by_path.values().filter(|r| r.status == PreviewStatus::Error).count();
        let unchanged = self.preview_by_path.values().filter(|r| r.status == PreviewStatus::Unchanged).count();
        let total = self.preview_by_path.len();
        // 节点总数（树全节点，含根目录）
        let mut node_count = 0usize;
        let mut stack2: Vec<&TreeNode> = vec![self.tree.as_ref().unwrap()];
        while let Some(n) = stack2.pop() {
            node_count += 1;
            stack2.extend(n.children.iter());
        }
        let skipped = node_count.saturating_sub(total);
        self.preview_stats = format!(
            "共 {node_count} 个节点 | 可改名 {total} | 将重命名 {ok} | 冲突 {conflict} | 错误 {error} | 无变化 {unchanged} | 跳过 {skipped}"
        );
        // 注意：不再写 status_msg，避免覆盖「改名成功/撤销」等操作反馈
    }

    fn apply(&mut self) {
        if self.tree.is_none() {
            self.status_msg = "请先加载目录".to_string();
            return;
        }
        let rules: Vec<power_rename::rules::Rule> = self.rules.iter().map(|r| r.to_rule()).collect();
        let mut entries = Vec::new();
        if let Some(tree) = &self.tree {
            let mut nodes = Vec::new();
            flatten_tree(tree, &mut nodes);
            for n in &nodes {
                if n.renameable {
                    entries.push(power_rename::fs_tree::FileEntry {
                        path: n.path.clone(),
                        name: n.name.clone(),
                        is_dir: n.is_dir,
                    });
                }
            }
        }
        let items = compute_preview(&entries, &rules);
        let mut todo: Vec<(PathBuf, PathBuf)> = Vec::new();
        for it in &items {
            if it.status == PreviewStatus::Ok && it.new_name != it.old_name {
                todo.push((it.entry.path.clone(), it.entry.path.parent().unwrap_or(Path::new("")).join(&it.new_name)));
            }
        }
        if todo.is_empty() {
            self.status_msg = "没有可执行的改名".to_string();
            return;
        }
        let res = apply_renames(&todo);
        if res.rolled_back {
            self.status_msg = format!("改名失败，已回滚：{}", res.errors.join("；"));
        } else {
            self.undo.push(res.logs.clone());
            self.status_msg = format!("成功改名 {} 项，可撤销", res.logs.len());
        }
        self.reload();
    }

    fn undo(&mut self) {
        let (done, errors) = self.undo.undo();
        if done > 0 {
            self.status_msg = format!("已撤销 {done} 项");
        } else if !errors.is_empty() {
            self.status_msg = format!("撤销失败：{}", errors.join("；"));
        } else {
            self.status_msg = "没有可撤销的操作".to_string();
        }
        self.reload();
    }

    /// 追加一条规则并选中（供添加按钮统一调用）。
    fn push_form(&mut self, form: RuleForm) {
        self.rules.push(form);
        self.selected_rule = Some(self.rules.len() - 1);
    }

    /// 演示规则集（仅供 PR_DEMO 验收截图）：依次应用会呈现出正常改名、
    /// 冲突与无变化三种预览状态，便于截图直观展示界面。
    fn demo_rules() -> Vec<RuleForm> {
        let mut rules = Vec::new();
        // 1) 前缀加 2026-：所有文件都会改名（正常状态）
        rules.push(RuleForm::Prefix { text: "2026-".to_string() });
        // 2) 正则 .txt → .md：存在冲突/无变化空间
        rules.push(RuleForm::Regex {
            pattern: r"\.txt$".to_string(),
            replace: ".md".to_string(),
            scope: 0,
        });
        rules
    }

    fn export_list(&mut self) {
        // 收集当前可改名条目（树中 renameable 节点）
        let mut entries = Vec::new();
        if let Some(tree) = &self.tree {
            let mut nodes = Vec::new();
            flatten_tree(tree, &mut nodes);
            for n in &nodes {
                if n.renameable {
                    entries.push(power_rename::fs_tree::FileEntry {
                        path: n.path.clone(),
                        name: n.name.clone(),
                        is_dir: n.is_dir,
                    });
                }
            }
        }
        if entries.is_empty() {
            self.status_msg = "没有可导出的条目（先加载目录）".to_string();
            return;
        }
        let rules: Vec<power_rename::rules::Rule> = self.rules.iter().map(|r| r.to_rule()).collect();
        let text = power_rename::list_io::build_export_text(&entries, &rules);
        let Some(path) = rfd::FileDialog::new()
            .add_filter("CSV", &["csv"])
            .set_file_name("rename_list.csv")
            .save_file()
        else {
            return;
        };
        // UTF-8 BOM，Excel 中文不乱码
        let mut data = vec![0xEF, 0xBB, 0xBF];
        data.extend_from_slice(text.as_bytes());
        match std::fs::write(&path, &data) {
            Ok(()) => self.status_msg = format!("已导出 {} 条到 {}", entries.len(), path.display()),
            Err(e) => self.status_msg = format!("导出失败：{e}"),
        }
    }

    fn import_list(&mut self) {
        let Some(path) = rfd::FileDialog::new()
            .add_filter("文本/CSV", &["csv", "txt"])
            .pick_file()
        else {
            return;
        };
        let raw = match std::fs::read(&path) {
            Ok(b) => b,
            Err(e) => {
                self.status_msg = format!("读取失败：{e}");
                return;
            }
        };
        // 编码探测：UTF-8 BOM → UTF-8；否则尝试 UTF-8，失败回退 GBK（Windows 常见）
        let text = decode_text(&raw);
        let mapping = power_rename::list_io::parse_rename_list(&text);
        if mapping.is_empty() {
            self.status_msg = "清单为空或格式无法识别".to_string();
            return;
        }
        // 追加一条清单规则（已有则替换最新一条）
        self.rules.push(RuleForm::List {
            mapping: mapping.clone(),
        });
        self.selected_rule = Some(self.rules.len() - 1);
        self.status_msg = format!("已导入 {} 条映射", mapping.len());
        self.reload();
    }
}

impl eframe::App for RenameApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        egui::TopBottomPanel::top("top").frame(panel_frame()).show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label("目录：");
                ui.add(
                    egui::TextEdit::singleline(&mut self.dir_input)
                        .desired_width(320.0)
                        .hint_text("输入文件夹路径"),
                );
                if ui.button("加载").clicked() {
                    self.reload();
                }
                if ui.button("浏览…").clicked() {
                    if let Some(dir) = rfd::FileDialog::new().pick_folder() {
                        self.dir_input = dir.to_string_lossy().into_owned();
                        self.reload();
                    }
                }
                ui.separator();
                if ui.button("导出清单").clicked() {
                    self.export_list();
                }
                if ui.button("导入清单").clicked() {
                    self.import_list();
                }
            });
            ui.horizontal(|ui| {
                let mut opts_changed = false;
                opts_changed |= ui.checkbox(&mut self.recursive, "递归子目录").changed();
                opts_changed |= ui.checkbox(&mut self.include_files, "包含文件").changed();
                opts_changed |= ui.checkbox(&mut self.include_dirs, "包含文件夹").changed();
                ui.separator();
                ui.label("名称包含：");
                opts_changed |= ui.add(
                    egui::TextEdit::singleline(&mut self.inc_text).desired_width(120.0)
                ).changed();
                ui.label("排除：");
                opts_changed |= ui.add(
                    egui::TextEdit::singleline(&mut self.exc_text).desired_width(120.0)
                ).changed();
                opts_changed |= ui.checkbox(&mut self.use_regex, "正则").changed();
                if opts_changed && self.tree.is_some() {
                    self.reload();
                }
            });
            // 预览统计信息（共 x 节点 | 可改名 x ...），弱化显示不抢顶部操作区视觉
            if !self.preview_stats.is_empty() {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new(&self.preview_stats).weak());
                });
            }
        });

        // 底部状态栏（应用/撤销按钮已移至规则面板）
        egui::TopBottomPanel::bottom("bottom").frame(panel_frame()).show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label(&self.status_msg);
            });
        });

        egui::SidePanel::left("rules").resizable(true).default_width(300.0).frame(panel_frame()).show(ctx, |ui| {
            // 添加规则：单个下拉菜单收拢全部 9 种规则（不再占三行按钮）
            ui.horizontal_wrapped(|ui| {
                ui.menu_button("+ 添加规则", |ui| {
                    if ui.button("查找替换").clicked() {
                        self.push_form(RuleForm::Replace {
                            search: String::new(),
                            replace: String::new(),
                            case_sensitive: false,
                            scope: 0,
                        });
                        ui.close_menu();
                    }
                    if ui.button("正则替换").clicked() {
                        self.push_form(RuleForm::Regex {
                            pattern: String::new(),
                            replace: String::new(),
                            scope: 0,
                        });
                        ui.close_menu();
                    }
                    if ui.button("大小写转换").clicked() {
                        self.push_form(RuleForm::Case { mode: 0, scope: 0 });
                        ui.close_menu();
                    }
                    ui.separator();
                    if ui.button("添加前缀").clicked() {
                        self.push_form(RuleForm::Prefix { text: String::new() });
                        ui.close_menu();
                    }
                    if ui.button("添加后缀").clicked() {
                        self.push_form(RuleForm::Suffix { text: String::new() });
                        ui.close_menu();
                    }
                    if ui.button("序列编号").clicked() {
                        self.push_form(RuleForm::Number {
                            pos: 1,
                            start: "1".into(),
                            step: "1".into(),
                            digits: "2".into(),
                            sep: " ".into(),
                        });
                        ui.close_menu();
                    }
                    ui.separator();
                    if ui.button("替换扩展名").clicked() {
                        self.push_form(RuleForm::Ext { text: "txt".into() });
                        ui.close_menu();
                    }
                    if ui.button("移除字符").clicked() {
                        self.push_form(RuleForm::Strip {
                            chars: "-_".into(),
                            scope: 0,
                        });
                        ui.close_menu();
                    }
                    if ui.button("压缩空白").clicked() {
                        self.push_form(RuleForm::Trim { underscore: false });
                        ui.close_menu();
                    }
                    if ui.button("按清单重命名").clicked() {
                        self.push_form(RuleForm::List {
                            mapping: std::collections::HashMap::new(),
                        });
                        ui.close_menu();
                    }
                });
                // 应用/撤销：放「+ 添加规则」右边同一行，方便改完规则直接操作
                ui.separator();
                if ui.button("应用").clicked() {
                    self.apply();
                }
                if ui.button("撤销").clicked() {
                    self.undo();
                }
            });
            // 管理按钮独立一行（删除/排序/清空），窄面板下自动换行；统一按钮宽度对齐
            ui.horizontal_wrapped(|ui| {
                if ui.add_sized([50.0, 24.0], egui::Button::new("删除")).clicked() {
                    if let Some(idx) = self.selected_rule {
                        if idx < self.rules.len() {
                            self.rules.remove(idx);
                            self.selected_rule = None;
                            self.reload();
                        }
                    }
                }
                if ui.add_sized([30.0, 24.0], egui::Button::new("↑")).clicked() {
                    if let Some(idx) = self.selected_rule {
                        if idx > 0 && idx < self.rules.len() {
                            self.rules.swap(idx, idx - 1);
                            self.selected_rule = Some(idx - 1);
                            self.reload();
                        }
                    }
                }
                if ui.add_sized([30.0, 24.0], egui::Button::new("↓")).clicked() {
                    if let Some(idx) = self.selected_rule {
                        if idx + 1 < self.rules.len() {
                            self.rules.swap(idx, idx + 1);
                            self.selected_rule = Some(idx + 1);
                            self.reload();
                        }
                    }
                }
                if ui.add_sized([50.0, 24.0], egui::Button::new("清空")).clicked() {
                    if !self.rules.is_empty() {
                        self.rules.clear();
                        self.selected_rule = None;
                        self.reload();
                    }
                }
            });

            // 规则列表
            let mut to_select: Option<usize> = None;
            for (i, r) in self.rules.iter().enumerate() {
                let selected = self.selected_rule == Some(i);
                if ui.selectable_label(selected, r.summary()).clicked() {
                    to_select = Some(i);
                }
            }
            if let Some(i) = to_select {
                self.selected_rule = Some(i);
            }

            ui.separator();

            // 规则表单
            if let Some(idx) = self.selected_rule {
                if idx < self.rules.len() {
                    ui.label(format!("规则 #{}", idx + 1));
                    let mut changed = false;
                    match &mut self.rules[idx] {
                        RuleForm::Replace { search, replace, case_sensitive, .. } => {
                            ui.horizontal(|ui| {
                                ui.label("查找：");
                                changed |= ui.text_edit_singleline(search).changed();
                            });
                            ui.horizontal(|ui| {
                                ui.label("替换为：");
                                changed |= ui.text_edit_singleline(replace).changed();
                            });
                            changed |= ui.checkbox(case_sensitive, "大小写敏感").changed();
                        }
                        RuleForm::Regex { pattern, replace, .. } => {
                            ui.horizontal(|ui| {
                                ui.label("正则：");
                                changed |= ui.text_edit_singleline(pattern).changed();
                            });
                            ui.horizontal(|ui| {
                                ui.label("替换为：");
                                changed |= ui.text_edit_singleline(replace).changed();
                            });
                        }
                        RuleForm::Case { mode, .. } => {
                            ui.label("转换方式：");
                            ui.horizontal_wrapped(|ui| {
                                for (mi, label) in ["小写", "大写", "首字母大写", "仅首字符大写"].iter().enumerate() {
                                    if ui.selectable_label(*mode == mi, *label).clicked() {
                                        *mode = mi;
                                        changed = true;
                                    }
                                }
                            });
                        }
                        RuleForm::Prefix { text } => {
                            ui.horizontal(|ui| {
                                ui.label("前缀文本：");
                                changed |= ui.text_edit_singleline(text).changed();
                            });
                        }
                        RuleForm::Suffix { text } => {
                            ui.horizontal(|ui| {
                                ui.label("后缀文本：");
                                changed |= ui.text_edit_singleline(text).changed();
                            });
                        }
                        RuleForm::Number { pos, start, step, digits, sep } => {
                            ui.label("位置：");
                            ui.horizontal_wrapped(|ui| {
                                for (pi, label) in ["前缀", "后缀"].iter().enumerate() {
                                    if ui.selectable_label(*pos == pi, *label).clicked() {
                                        *pos = pi;
                                        changed = true;
                                    }
                                }
                            });
                            ui.horizontal_wrapped(|ui| {
                                ui.label("起始值：");
                                changed |= ui.text_edit_singleline(start).changed();
                                ui.label("步长：");
                                changed |= ui.text_edit_singleline(step).changed();
                            });
                            ui.horizontal_wrapped(|ui| {
                                ui.label("位数：");
                                changed |= ui.text_edit_singleline(digits).changed();
                                ui.label("分隔符：");
                                changed |= ui.text_edit_singleline(sep).changed();
                            });
                        }
                        RuleForm::Ext { text } => {
                            ui.horizontal(|ui| {
                                ui.label("新扩展名：");
                                changed |= ui.text_edit_singleline(text).changed();
                            });
                        }
                        RuleForm::Strip { chars, .. } => {
                            ui.horizontal(|ui| {
                                ui.label("移除字符：");
                                changed |= ui.text_edit_singleline(chars).changed();
                            });
                        }
                        RuleForm::Trim { underscore } => {
                            changed |= ui.checkbox(underscore, "用下划线代替空格").changed();
                        }
                        RuleForm::List { mapping } => {
                            ui.label(format!("按清单重命名：{} 条映射（导入自 CSV/文本）", mapping.len()));
                            ui.label("清单规则按「原始文件名」匹配，命中则采用清单新名。");
                            if ui.button("查看映射…").clicked() {
                                self.mapping_view = Some(idx);
                            }
                        }
                    }
                    // 作用范围（仅带 scope 的规则适用；List/Prefix/Suffix/Number/Ext/Trim 不适用）
                    if matches!(self.rules[idx], RuleForm::Replace { .. }
                        | RuleForm::Regex { .. }
                        | RuleForm::Case { .. }
                        | RuleForm::Strip { .. })
                    {
                        let scopes = ["完整文件名", "主名（不含扩展名）", "扩展名"];
                        let mut scope = self.rules[idx].scope();
                        ui.label("作用范围：");
                        ui.horizontal_wrapped(|ui| {
                            for (si, label) in scopes.iter().enumerate() {
                                if ui.selectable_label(scope == si, *label).clicked() {
                                    scope = si;
                                    changed = true;
                                }
                            }
                        });
                        self.rules[idx].set_scope(scope);
                    }

                    // 规则变化 → 实时刷新预览
                    if changed {
                        self.reload();
                    }
                }
            } else if self.rules.is_empty() {
                // 无规则：居中大字水印（替代原「（还没有规则）」提示）
                ui.centered_and_justified(|ui| {
                    ui.label(egui::RichText::new("规则").size(48.0).color(egui::Color32::from_gray(0xBB)));
                });
                ui.add_space(12.0);
            } else {
                ui.label("（选择一条规则编辑）");
            }
        });

        // 预览右键菜单待执行动作（渲染时收集，面板结束后统一处理）
        let mut action: PreviewAction = PreviewAction::None;

        egui::CentralPanel::default().frame(panel_frame()).show(ctx, |ui| {
            if let Some(tree) = &self.tree {
                // ---- 预览区 Ctrl+滚轮缩放：只作用于预览表格（字号/行高/列宽/缩进） ----
                // 鼠标须在预览区上方才响应（避免缩放规则面板以外的区域）；滚动事件自带 ctrl 修饰时
                // 清掉平滑滚动，阻止同一事件既缩放又滚动滚动条。
                if ui.rect_contains_pointer(ui.clip_rect()) {
                    let mut zoom_delta = 0.0;
                    ui.input(|i| {
                        for ev in &i.events {
                            if let egui::Event::MouseWheel { unit, delta, modifiers } = ev {
                                if modifiers.ctrl {
                                    let per = match unit {
                                        egui::MouseWheelUnit::Point => 0.002,
                                        egui::MouseWheelUnit::Line => 0.1,
                                        egui::MouseWheelUnit::Page => 0.5,
                                    };
                                    zoom_delta += delta.y * per;
                                }
                            }
                        }
                    });
                    if zoom_delta != 0.0 {
                        self.preview_zoom = (self.preview_zoom + zoom_delta).clamp(0.5, 2.0);
                        // 清掉同一事件的平滑滚动，避免缩放同时滚动滚动条
                        ui.input_mut(|i| i.smooth_scroll_delta = egui::Vec2::ZERO);
                    }
                    // zoom 变化时表格 state 重建，列宽按缩放后的 initial 重新计算
                    // （可拖拽列会锁存上一帧宽度，id_salt 变更后强制重算）
                }
                let zoom = self.preview_zoom;
                // 预览区局部字号：放大/缩小，不影响规则面板与全局
                {
                    let style = ui.style_mut();
                    for (ts, font_id) in &mut style.text_styles {
                        if matches!(ts, egui::TextStyle::Body | egui::TextStyle::Button | egui::TextStyle::Monospace) {
                            font_id.size = 17.0 * zoom;
                        }
                    }
                }
                // 预览框右上角：显示当前缩放比例（固定字号，不随预览缩放、不抢注意）
                // 左侧：状态筛选按钮（计数实时从 preview_by_path 统计）
                let filter = self.preview_filter;
                let counts = {
                    let mut c = [0usize; 5]; // [全部, Ok, Unchanged, Conflict, Error]
                    c[0] = self.preview_by_path.len();
                    for r in self.preview_by_path.values() {
                        match r.status {
                            PreviewStatus::Ok => c[1] += 1,
                            PreviewStatus::Unchanged => c[2] += 1,
                            PreviewStatus::Conflict => c[3] += 1,
                            PreviewStatus::Error => c[4] += 1,
                        }
                    }
                    c
                };
                // 筛选按钮：全部/就绪/无变化/冲突/错误（None=全部）。单击切换、再点取消；
                // 计数为 0 的按钮禁用（「全部」恒可用）。选中态用 Button::selected 高亮。
                let mut new_filter = filter;
                ui.horizontal(|ui| {
                    let mut btn = |ui: &mut egui::Ui, label: &str, st: Option<PreviewStatus>, cnt: usize| {
                        let sel = filter == st;
                        if cnt == 0 && st.is_some() {
                            return; // 该状态无条目，不显示按钮（「全部」恒显示）
                        }
                        let text = if sel {
                            egui::RichText::new(format!("{label} {cnt}"))
                                .strong()
                                .color(egui::Color32::WHITE)
                        } else {
                            egui::RichText::new(format!("{label} {cnt}"))
                        };
                        let resp = ui.add(egui::Button::new(text).selected(sel).small());
                        if resp.clicked() {
                            new_filter = if sel { None } else { st };
                        }
                    };
                    btn(ui, "全部", None, counts[0]);
                    ui.separator();
                    btn(ui, "就绪", Some(PreviewStatus::Ok), counts[1]);
                    btn(ui, "无变化", Some(PreviewStatus::Unchanged), counts[2]);
                    btn(ui, "冲突", Some(PreviewStatus::Conflict), counts[3]);
                    btn(ui, "错误", Some(PreviewStatus::Error), counts[4]);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(
                            egui::RichText::new(format!("缩放 {}%", (zoom * 100.0).round() as i32))
                                .weak()
                                .size(13.0),
                        );
                    });
                });
                // 应用到 self（避免水平闭包内可变借用冲突）
                self.preview_filter = new_filter;

                // 树感知过滤：收集命中路径 → 向父目录逐级扩散（祖先链）
                // 目录行在筛选中显示 = 自身命中 或 子树有命中；命中目录自动展开
                let filter = self.preview_filter;
                let mut subtree_hit: std::collections::HashSet<std::path::PathBuf> = std::collections::HashSet::new();
                if let Some(f) = filter {
                    subtree_hit.extend(
                        self.preview_by_path
                            .iter()
                            .filter(|(_, r)| r.status == f)
                            .map(|(p, _)| p.clone()),
                    );
                    // 逐级向父目录扩散：祖先链上的目录都会被标记为「子树有命中」
                    let mut changed = true;
                    while changed {
                        changed = false;
                        for p in subtree_hit.clone() {
                            if let Some(parent) = p.parent() {
                                if parent != p && subtree_hit.insert(parent.to_path_buf()) {
                                    changed = true;
                                }
                            }
                        }
                    }
                    // 命中目录自动展开祖先链，确保筛选结果可见
                    self.expanded.extend(subtree_hit.iter().filter(|p| p.is_dir()).cloned());
                }
                // 多列表格：当前名称（树形缩进）/ 新名称 / 状态 / 说明
                // 包一层双向滚动区：列总宽超过面板宽度时可左右滚动
                use egui_extras::{Column, TableBuilder};
                egui::ScrollArea::both()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        TableBuilder::new(ui)
                            // 列宽可拉伸，min_scrolled_width 保证滚动区有最小内容宽
                            .min_scrolled_height(300.0)
                            .striped(true)
                            .resizable(true)
                            // zoom 变化时重建表格状态（列宽锁存首帧，id_salt 变更强制重算）
                            .id_salt(("preview_table", zoom.to_bits()))
                            .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
                            // 四列全部可拖拽调宽：resizable(true) 启用拖拽，
                            // clip(true) 允许缩窄到内容宽度以下（否则长文件名会撑住最小宽）。
                            // 注意：最后一列不能用 remainder()，remainder 列会被强制填充剩余
                            // 空间并跳过拖拽逻辑（egui_extras 限制），故说明列也用 initial 可拖。
                            .column(Column::initial(300.0 * zoom).at_least(120.0 * zoom).clip(true).resizable(true)) // 当前名称
                            .column(Column::initial(260.0 * zoom).at_least(100.0 * zoom).clip(true).resizable(true)) // 新名称
                            .column(Column::initial(70.0 * zoom).at_least(50.0 * zoom).clip(true).resizable(true))    // 状态
                            .column(Column::initial(300.0 * zoom).at_least(120.0 * zoom).clip(true).resizable(true)) // 说明
                            // 行高跟随真实字体行高：galley 单行（Label 不换行）+ 上下各 2px 冗余，
                            // 与高亮背景计算同源（选中/hover 背景 expand2 到 gapless），任何倍率不遮挡
                            .header(26.0 * zoom, |mut header| {
                                header.col(|ui| {
                                    ui.strong("当前名称（结构）");
                                });
                                header.col(|ui| {
                                    ui.strong("新名称");
                                });
                                header.col(|ui| {
                                    ui.strong("状态");
                                });
                                header.col(|ui| {
                                    ui.strong("说明");
                                    // 表头右键：刷新预览
                                    if ui.response().secondary_clicked() {
                                        action = PreviewAction::Refresh;
                                    }
                                });
                            })
                            .body(|mut body| {
                                // 根目录恒可见（且默认展开）；子节点仅当其父目录展开时才渲染
                                render_tree_rows(&mut body, tree, &self.preview_by_path, &mut self.expanded, &mut action, &mut self.selected_preview, zoom, true, true, 0, filter, &subtree_hit);
                                if tree.children.is_empty() {
                                    body.row(26.0 * zoom, |mut row| {
                                        row.col(|ui| {
                                            ui.label("（空目录）");
                                        });
                                        row.col(|_ui| {});
                                        row.col(|_ui| {});
                                        row.col(|_ui| {});
                                    });
                                }
                            });
                    });
            } else {
                // 未加载目录：居中大字水印
                ui.centered_and_justified(|ui| {
                    ui.label(egui::RichText::new("预览").size(60.0).color(egui::Color32::from_gray(0xBB)));
                });
            }
        });

        // 清单映射查看窗口
        if let Some(idx) = self.mapping_view {
            if idx < self.rules.len() {
                if let RuleForm::List { mapping } = &self.rules[idx] {
                    let mut open = true;
                    egui::Window::new(format!("清单映射（规则 #{}）", idx + 1))
                        .open(&mut open)
                        .default_size([360.0, 400.0])
                        .show(ctx, |ui| {
                            ui.label(format!("共 {} 条映射：", mapping.len()));
                            ui.separator();
                            egui::ScrollArea::vertical().auto_shrink([false, false]).max_height(340.0).show(ui, |ui| {
                                // 按原名排序展示（稳定）
                                let mut pairs: Vec<(&String, &String)> = mapping.iter().collect();
                                pairs.sort_by(|a, b| a.0.cmp(b.0));
                                for (from, to) in pairs {
                                    ui.horizontal(|ui| {
                                        ui.label(format!("{from}  →  {to}"));
                                    });
                                }
                            });
                        });
                    if !open {
                        self.mapping_view = None;
                    }
                } else {
                    // 规则类型变化（如被替换）→ 关闭
                    self.mapping_view = None;
                }
            } else {
                self.mapping_view = None;
            }
        }

        // 处理右键菜单动作
        match action {
            PreviewAction::Open(path) => {
                let _ = std::process::Command::new("explorer").arg(&path).spawn();
            }
            PreviewAction::Reveal(path) => {
                // 打开所在文件夹并选中文件（Windows: explorer /select,）
                let _ = std::process::Command::new("explorer").args(["/select,", &path.to_string_lossy()]).spawn();
            }
            PreviewAction::Refresh => self.reload(),
            PreviewAction::None => {}
        }

        // 截图钩子（仅供验收）：PR_CAPTURE 指定路径时，等界面稳定（约 60 帧）后请求
        // egui 自截图一帧并保存——过早请求会截到未完成布局/表格未填充完的帧。
        if let Some(out) = self.capture_path.clone() {
            self.frame_count += 1;
            if self.frame_count < 60 {
                // 无前台窗口时 egui 空闲不重绘，必须显式要求继续绘制才能推进帧计数
                ctx.request_repaint();
            } else if !self.capture_sent {
                self.capture_sent = true;
                ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::default()));
            }
            if let Some(image) = ctx.input(|i| i.events.iter().find_map(|e| match e {
                egui::Event::Screenshot { image, .. } => Some(image.clone()),
                _ => None,
            })) {
                // 写 BMP（低依赖：手写 24-bit 位图，仅 R/G/B）
                let w = image.size[0] as u32;
                let h = image.size[1] as u32;
                let row_bytes = (w * 3 + 3) & !3;
                let mut bmp = Vec::with_capacity(54 + (row_bytes * h) as usize);
                let file_size = 54 + row_bytes * h;
                bmp.extend_from_slice(b"BM");
                bmp.extend_from_slice(&file_size.to_le_bytes());
                bmp.extend_from_slice(&[0u8; 4]);          // reserved
                bmp.extend_from_slice(&54u32.to_le_bytes()); // pixel_data_offset
                bmp.extend_from_slice(&40u32.to_le_bytes()); // biSize
                bmp.extend_from_slice(&w.to_le_bytes());
                bmp.extend_from_slice(&h.to_le_bytes());
                bmp.extend_from_slice(&[1, 0]);            // planes=1
                bmp.extend_from_slice(&[24, 0]);           // bpp=24
                bmp.extend_from_slice(&[0u8; 24]);         // compression 0 + 其余为 0
                for y in 0..h {
                    let row_start = (h - 1 - y) as usize * w as usize; // bottom-up
                    for x in 0..w {
                        let p = image.pixels[row_start + x as usize];
                        bmp.push(p.b()); // BMP 是 BGR
                        bmp.push(p.g());
                        bmp.push(p.r());
                    }
                    // 行对齐到 4 字节
                    let pad = row_bytes - w * 3;
                    bmp.extend(std::iter::repeat(0u8).take(pad as usize));
                }
                if let Err(e) = std::fs::write(&out, &bmp) {
                    eprintln!("[PowerRename] 截图保存失败 {out:?}: {e}");
                }
                std::process::exit(0);
            }
        }
    }
}

/// 递归把树渲染进表格 body。
///
/// 目录行：第一列显示缩进 + painter 绘制的折叠三角 + 名字；
/// - 单击折叠三角 → 切换展开/折叠（三角与名字职责分离，避免双击目录名时
///   第一击先触发折叠、缩起整棵子树再弹资源管理器）；
/// - 双击目录名 → 打开文件夹。
/// 文件行：双击 → 打开所在文件夹并定位文件。
/// 表头右键 → 刷新预览。

/// 自绘整行 hover 背景：指针悬停在 cell 内 → 画淡蓝填充（与选中同色系、更浅）。
/// 每格各画自己的区域（与 striped/selected 同款 expand2 无缝拼接），拼合即整行；
/// 即时按指针位置判断，无延迟、不走官方 hover 的延迟帧缓存（官方通道为灰色且不稳定）。
/// 已选中行不再叠加 hover（保持完整蓝条）。
fn paint_row_hover(ui: &egui::Ui, is_selected: bool) {
    if is_selected || !ui.ctx().rect_contains_pointer(ui.layer_id(), ui.max_rect()) {
        return;
    }
    // 淡蓝 hover（≈ 主题 weak_bg_fill 0xD7E3F8 附近）；选中完整蓝条 0xCBDDF5 更深
    ui.painter().rect_filled(
        ui.max_rect().expand2(0.5 * ui.spacing().item_spacing),
        egui::CornerRadius::ZERO,
        egui::Color32::from_rgb(0xE0, 0xEB, 0xFA),
    );
}

fn render_tree_rows(
    body: &mut egui_extras::TableBody,
    node: &power_rename::fs_tree::TreeNode,
    by_path: &std::collections::HashMap<std::path::PathBuf, PreviewRow>,
    expanded: &mut std::collections::HashSet<std::path::PathBuf>,
    action: &mut PreviewAction,
    selected_preview: &mut Option<std::path::PathBuf>,
    zoom: f32,
    visible: bool,
    default_open: bool,
    depth: usize,
    filter: Option<PreviewStatus>,
    subtree_hit: &std::collections::HashSet<std::path::PathBuf>,
) {
    if !visible {
        return;
    }

    if node.is_dir {
        // 状态筛选：目录行命中 = 自身是目标状态 或 子树里有目标状态。
        // 未命中的目录整行隐藏（其子节点也随递归一并隐藏），保持树的层级语义。
        if let Some(f) = filter {
            let self_hit = by_path.get(&node.path).map(|r| r.status == f).unwrap_or(false);
            if !self_hit && !subtree_hit.contains(&node.path) {
                return;
            }
        }
        let is_open = expanded.contains(&node.path) || default_open;
        // 目录行的新名/状态/说明：勾选「包含文件夹」且目录可改名时，
        // preview_by_path 中会有该目录的预览结果，此时后三列与文件行一致显示；
        // 未命中（根目录、未勾选包含文件夹）则留空，保持树形结构展示。
        let dir_preview = by_path.get(&node.path);
        let is_dir_skipped = dir_preview.is_none();
        let (dir_new_name, dir_status, dir_note) = match dir_preview {
            Some(r) => (r.new_name.clone(), r.status, r.note.clone()),
            None => (String::new(), PreviewStatus::Unchanged, String::new()),
        };
        let dir_color = if is_dir_skipped {
            egui::Color32::from_rgb(0xA0, 0xA0, 0xA0) // 跳过（未参与改名）→ 浅灰
        } else {
            match dir_status {
                PreviewStatus::Ok => egui::Color32::from_rgb(0x2e, 0x8b, 0x57),
                PreviewStatus::Conflict => egui::Color32::from_rgb(0xc0, 0x39, 0x2b),
                PreviewStatus::Error => egui::Color32::from_rgb(0x8b, 0x00, 0x00),
                PreviewStatus::Unchanged => egui::Color32::GRAY,
            }
        };
        let dir_status_label = if is_dir_skipped {
            "跳过"
        } else {
            match dir_status {
                PreviewStatus::Ok => "就绪",
                PreviewStatus::Unchanged => "无变化",
                PreviewStatus::Conflict => "冲突",
                PreviewStatus::Error => "错误",
            }
        };
        body.row(26.0 * zoom, |mut row| {
            // 整行高亮：选中用 set_selected 画整行 selection 背景（在文字之下、跨 cell 无缝）；
            // hover 不依赖官方延迟通道（灰色且不稳定），改为每格自绘淡蓝（paint_row_hover）
            let is_selected = selected_preview.as_ref() == Some(&node.path);
            row.set_selected(is_selected);
            row.col(|ui| {
                paint_row_hover(ui, is_selected);
                ui.horizontal(|ui| {
                    ui.add_space(depth as f32 * 16.0 * zoom);
                    // 折叠三角用 painter 直接绘制（▸/▾ 等字符在中文系统字体中
                    // 无字形会显示成问号；画出来的三角跨平台字体无关、永不出问号）
                    // 三角响应单击：折叠/展开切到这个三角上，目录名双击不再有折叠副作用。
                    // hover 时三角变强调蓝（与文件行文本色区分）并加浅蓝底提示可点击
                    // 尺寸随 zoom 缩放，保持与文字比例一致
                    let (tri_rect, tri_resp) = ui.allocate_exact_size(egui::vec2(12.0 * zoom, 12.0 * zoom), egui::Sense::click());
                    let painter = ui.painter();
                    let tri_color = if tri_resp.hovered() {
                        egui::Color32::from_rgb(0x2F, 0x6F, 0xD5) // 强调蓝：hover 高亮
                    } else {
                        ui.visuals().text_color()
                    };
                    if tri_resp.hovered() {
                        painter.rect_filled(tri_rect.expand2(egui::vec2(2.0, 1.0)), 2.0, egui::Color32::from_rgb(0xE8, 0xF0, 0xFB));
                    }
                    let c = tri_rect.center();
                    let s = 3.5 * zoom;
                    let tri: Vec<egui::Pos2> = if is_open {
                        // 展开：向下小三角 ▼
                        vec![
                            egui::pos2(c.x - s, c.y - s * 0.6),
                            egui::pos2(c.x + s, c.y - s * 0.6),
                            egui::pos2(c.x, c.y + s),
                        ]
                    } else {
                        // 折叠：向右小三角 ▶
                        vec![
                            egui::pos2(c.x - s, c.y - s),
                            egui::pos2(c.x - s, c.y + s),
                            egui::pos2(c.x + s, c.y),
                        ]
                    };
                    painter.add(egui::Shape::convex_polygon(tri, tri_color, egui::Stroke::NONE));
                    if tri_resp.clicked() {
                        let next = !is_open;
                        if next {
                            expanded.insert(node.path.clone());
                        } else {
                            expanded.remove(&node.path);
                        }
                    }
                    // 目录名：单击选中该行（持久高亮），双击打开文件夹。
                    // 用带点击感应的 label（不再自绘局部高亮矩形，整行高亮由 set_selected 负责）
                    let resp = ui.add(egui::Label::new(&node.name).sense(egui::Sense::click()));
                    if resp.clicked() {
                        *selected_preview = Some(node.path.clone());
                    }
                    if resp.double_clicked() {
                        *action = PreviewAction::Open(node.path.clone());
                    }
                });
            });
            row.col(|ui| {
                // 目录新名称（参与改名时显示）
                paint_row_hover(ui, is_selected);
                if !is_dir_skipped {
                    ui.colored_label(dir_color, dir_new_name);
                }
            });
            row.col(|ui| {
                // 目录状态
                paint_row_hover(ui, is_selected);
                if !is_dir_skipped {
                    ui.label(dir_status_label);
                }
            });
            row.col(|ui| {
                // 目录说明
                paint_row_hover(ui, is_selected);
                if !is_dir_skipped && !dir_note.is_empty() {
                    ui.label(dir_note);
                }
            });
        });
        if is_open {
            for child in &node.children {
                render_tree_rows(body, child, by_path, expanded, action, selected_preview, zoom, true, false, depth + 1, filter, subtree_hit);
            }
        }
        return;
    }

    // 文件行
    let row_info = by_path.get(&node.path);
    // 状态筛选：文件行命中 = 参与改名（跳过行一律隐藏）且状态匹配目标
    if let Some(f) = filter {
        if row_info.map(|r| r.status == f).unwrap_or(false) != true {
            return;
        }
    }
    // 不在预览映射 → 该节点被筛选跳过（不可改名），显示「跳过」标签
    let is_skipped = row_info.is_none();
    let (new_name, status, note) = match row_info {
        Some(r) => (r.new_name.clone(), r.status, r.note.clone()),
        None => (String::new(), PreviewStatus::Unchanged, String::new()),
    };
    let color = if is_skipped {
        egui::Color32::from_rgb(0xA0, 0xA0, 0xA0) // 更浅的灰
    } else {
        match status {
            PreviewStatus::Ok => egui::Color32::from_rgb(0x2e, 0x8b, 0x57),
            PreviewStatus::Conflict => egui::Color32::from_rgb(0xc0, 0x39, 0x2b),
            PreviewStatus::Error => egui::Color32::from_rgb(0x8b, 0x00, 0x00),
            PreviewStatus::Unchanged => egui::Color32::GRAY,
        }
    };
    let status_label = if is_skipped {
        "跳过"
    } else {
        match status {
            PreviewStatus::Ok => "就绪",
            PreviewStatus::Unchanged => "无变化",
            PreviewStatus::Conflict => "冲突",
            PreviewStatus::Error => "错误",
        }
    };
    body.row(26.0 * zoom, |mut row| {
        // 整行高亮：选中用 set_selected（跨 cell 无缝、在文字之下）；
        // hover 不依赖官方延迟通道，改为每格自绘淡蓝（paint_row_hover）
        let is_selected = selected_preview.as_ref() == Some(&node.path);
        row.set_selected(is_selected);
        row.col(|ui| {
            paint_row_hover(ui, is_selected);
            ui.horizontal(|ui| {
                ui.add_space(depth as f32 * 16.0 * zoom);
                // 带点击感应的 label：单击选中该行（持久），双击打开所在文件夹并定位；
                // 整行 hover 由 self 自绘（paint_row_hover），label 不再自绘局部矩形
                let label = ui.add(egui::Label::new(egui::RichText::new(&node.name).color(color)).sense(egui::Sense::click()));
                // 左键单击：选中该行（持久高亮）
                if label.clicked() {
                    *selected_preview = Some(node.path.clone());
                }
                // 左键双击：打开所在文件夹并定位文件
                if label.double_clicked() {
                    *action = PreviewAction::Reveal(node.path.clone());
                }
            });
        });
        row.col(|ui| {
            paint_row_hover(ui, is_selected);
            if !is_skipped {
                ui.colored_label(color, new_name);
            }
        });
        row.col(|ui| {
            paint_row_hover(ui, is_selected);
            ui.label(status_label);
        });
        row.col(|ui| {
            paint_row_hover(ui, is_selected);
            if !is_skipped && !note.is_empty() {
                ui.label(note);
            }
        });
    });
}

fn main() -> eframe::Result {
    // 支持命令行参数：power_rename.exe <路径> 启动时直接加载该目录（args_os 保留中文路径）
    let initial_dir = std::env::args_os().nth(1) // 第 1 个参数（第 0 个是程序自身）
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string_lossy().into_owned());
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([1000.0, 620.0]).with_title("PowerRename — 批量重命名工具"),
        ..Default::default()
    };
    eframe::run_native(
        "PowerRename",
        options,
        Box::new(move |cc| {
            install_chinese_font(&cc.egui_ctx);
            install_light_theme(&cc.egui_ctx);
            let mut app = RenameApp::new();
            if let Some(dir) = initial_dir.as_ref() {
                app.dir_input = dir.clone();
                app.reload();
            }
            Ok(Box::new(app))
        }),
    )
}

/// 安装定制的浅色主题：浅灰背景、白色面板、控件带边框、按钮有悬停/按下反馈。
fn install_light_theme(ctx: &egui::Context) {
    let mut visuals = egui::Visuals::light();

    // 背景统一浅灰（面板/窗口/表格行都别纯白避免刺眼）；比之前再灰一档
    let bg = egui::Color32::from_rgb(0xE5, 0xE6, 0xE9);
    let bg_alt = egui::Color32::from_rgb(0xD9, 0xDB, 0xDF); // 表格交替行
    visuals.panel_fill = bg;
    visuals.window_fill = bg;
    visuals.extreme_bg_color = bg_alt;
    visuals.faint_bg_color = bg_alt; // TableBuilder striped 行

    // 控件边框 + 圆角，让按钮/输入框有轮廓
    let border = egui::Color32::from_rgb(0xC2, 0xC6, 0xCA);
    let accent = egui::Color32::from_rgb(0x2F, 0x6F, 0xD5); // 蓝色强调

    for w in [
        &mut visuals.widgets.inactive,
        &mut visuals.widgets.hovered,
        &mut visuals.widgets.active,
    ] {
        w.bg_stroke = egui::Stroke::new(1.0, border);
        w.corner_radius = egui::CornerRadius::same(4);
    }
    // 按钮/控件：常态浅蓝底（区别于输入框白底），悬停加深、按下更深，突出可点击
    visuals.widgets.inactive.bg_fill = egui::Color32::from_rgb(0xE9, 0xEF, 0xFA);
    visuals.widgets.hovered.bg_stroke = egui::Stroke::new(1.2, accent);
    visuals.widgets.hovered.weak_bg_fill = egui::Color32::from_rgb(0xD7, 0xE3, 0xF8);
    visuals.widgets.active.bg_stroke = egui::Stroke::new(1.5, accent);
    visuals.widgets.active.bg_fill = egui::Color32::from_rgb(0xC3, 0xD6, 0xF2);

    // 选中项背景
    visuals.selection.bg_fill = egui::Color32::from_rgb(0xCB, 0xDD, 0xF5);

    // 按钮稍微加大：只调内边距（字号不变，文本仍按原字号渲染）
    ctx.style_mut(|style| {
        // 全局字体放大两号：Body/Button/Monospace 14→17，Heading 20→24，Small 10→12
        for (ts, font_id) in &mut style.text_styles {
            let new_size = match ts {
                egui::TextStyle::Small => 12.0,
                egui::TextStyle::Body | egui::TextStyle::Button | egui::TextStyle::Monospace => 17.0,
                egui::TextStyle::Heading => 24.0,
                egui::TextStyle::Name(_) => font_id.size + 3.0,
            };
            font_id.size = new_size;
        }
        // 按钮等控件尺寸随字号放大：内边距加大 + 最小交互尺寸拉高
        style.spacing.button_padding = egui::vec2(10.0, 4.0);
        style.spacing.interact_size = egui::vec2(46.0, 22.0);
    });

    ctx.set_visuals(visuals);
}

/// 面板统一样式：接缝处微灰（与背景一致），内边距让内容不贴边。
fn panel_frame() -> egui::Frame {
    egui::Frame::new()
        .fill(egui::Color32::from_rgb(0xE5, 0xE6, 0xE9))
        .stroke(egui::Stroke::new(1.0, egui::Color32::from_rgb(0xCC, 0xCF, 0xD3)))
        .inner_margin(egui::Margin::same(8))
        .corner_radius(egui::CornerRadius::same(0))
}

/// 安装中文字体（egui 默认字体不含中文，需加载字体，否则中文显示为乱码/方框）。
///
/// 为什么 tkinter 没这问题：tkinter 不嵌入字体，直接用系统字体渲染文本；
/// egui 用内置字形图集（glyph atlas）绘制所有文本，必须把字体文件读进内存，
/// 否则非 English 字符（中文等）无字形可画 → 方框/乱码。
///
/// 采用「内嵌 gzip 压缩的文泉驿微米黑（OFL 自由许可，可随程序分发）」：
/// - 字体以 gzip 压缩形式 `include_bytes!` 进 exe（assets/fonts/wqy-microhei.ttc.gz，2.3MB），
///   运行时用 flate2 解压。相比「运行时读系统字体」的旧方案（Windows msyh / macOS PingFang /
///   Linux Noto CJK），可以彻底摆脱对系统装没装中文字体的依赖，三平台开箱即用；
///   比直接内嵌原始 .ttc（5.0MB）省约 2.7MB（压缩率 ~54%）。
/// - 代价：启动时一次性解压 2.3MB（毫秒级，用户无感）；exe 体积 2.9MB → ~5MB。
///
/// 注意：项目已关闭 egui 的 default_fonts feature（编译期不再嵌入 4 个内置字体
/// 以压缩体积），因此这里必须成功加载字体，否则界面完全无字形。
fn install_chinese_font(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();

    // 内嵌的 gzip 压缩子集字体（GB2312 全量 6763 汉字 + ASCII + 常用符号，~680KB gz）
    let compressed: &[u8] = include_bytes!("../assets/fonts/wqy-microhei-subset.ttf.gz");
    let embedded_ok = {
        use std::io::Read;
        let mut decoder = flate2::read::GzDecoder::new(compressed);
        let mut bytes: Vec<u8> = Vec::with_capacity(compressed.len() * 2);
        if decoder.read_to_end(&mut bytes).is_ok() && !bytes.is_empty() {
            let font_data = egui::FontData::from_owned(bytes);
            fonts.font_data.insert("main".to_owned(), font_data.into());
            true
        } else {
            false
        }
    };
    if embedded_ok {
        // 内嵌子集作为比例/等宽字体的首选字形
        fonts
            .families
            .entry(egui::FontFamily::Proportional)
            .or_default()
            .insert(0, "main".to_owned());
        fonts
            .families
            .entry(egui::FontFamily::Monospace)
            .or_default()
            .insert(0, "main".to_owned());
    } else {
        eprintln!("[PowerRename] 内嵌子集字体解压失败，仅回退系统字体");
    }

    // 系统 CJK 字体作为 fallback：覆盖子集未收录的生僻字（不依赖子集嵌入则作为主力）
    const SYSTEM_FONT_CANDIDATES: &[&str] = &[
        // Windows 微软雅黑/黑体/宋体/等线
        "C:\\Windows\\Fonts\\msyh.ttc",
        "C:\\Windows\\Fonts\\msyh.ttf",
        "C:\\Windows\\Fonts\\msyhbd.ttc",
        "C:\\Windows\\Fonts\\simhei.ttf",
        "C:\\Windows\\Fonts\\simsun.ttc",
        "C:\\Windows\\Fonts\\Deng.ttf",
        // macOS 苹方/华文黑体/宋体
        "/System/Library/Fonts/PingFang.ttc",
        "/System/Library/Fonts/Hiragino Sans GB.ttc",
        "/System/Library/Fonts/STHeiti Light.ttc",
        "/System/Library/Fonts/Supplemental/Songti.ttc",
        // Linux 思源黑体/文泉驿/Droid
        "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
        "/usr/share/fonts/opentype/noto/NotoSansCJKsc-Regular.otf",
        "/usr/share/fonts/truetype/wqy/wqy-microhei.ttc",
        "/usr/share/fonts/truetype/droid/DroidSansFallbackFull.ttf",
        "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
    ];
    for path in SYSTEM_FONT_CANDIDATES {
        if let Ok(bytes) = std::fs::read(path) {
            let font_data = egui::FontData::from_owned(bytes);
            fonts.font_data.insert("sys_cjk".to_owned(), font_data.into());
            // 排在主字体之后（内嵌失败则排首位），egui 按数组顺序查字形
            let idx = if embedded_ok { 1 } else { 0 };
            fonts
                .families
                .entry(egui::FontFamily::Proportional)
                .or_default()
                .insert(idx, "sys_cjk".to_owned());
            fonts
                .families
                .entry(egui::FontFamily::Monospace)
                .or_default()
                .insert(idx, "sys_cjk".to_owned());
            break;
        }
    }

    ctx.set_fonts(fonts);
}

/// 文本解码：UTF-8 BOM / UTF-8 优先，失败回退 GBK（Windows 常见中文编码）。
fn decode_text(raw: &[u8]) -> String {
    if raw.starts_with(&[0xEF, 0xBB, 0xBF]) {
        return String::from_utf8_lossy(&raw[3..]).into_owned();
    }
    match std::str::from_utf8(raw) {
        Ok(s) => s.to_string(),
        Err(_) => {
            // GBK 回退（encoding_rs 标准库）
            let (text, _, _) = encoding_rs::GBK.decode(raw);
            text.into_owned()
        }
    }
}
