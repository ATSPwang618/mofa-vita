//! Launcher copy; all pages share the same language choice.
use super::super::{CursorSpeed, Language, RenderQuality};

#[derive(Clone, Copy)]
pub(super) struct Text(pub Language);
impl Text {
    pub fn pick(self, zh: &'static str, en: &'static str, ja: &'static str) -> &'static str {
        match self.0 {
            Language::Chinese => zh,
            Language::English => en,
            Language::Japanese => ja,
        }
    }
    pub fn details(self) -> &'static str {
        self.pick("游戏设置", "Game settings", "ゲーム設定")
    }
    pub fn settings(self) -> &'static str {
        self.pick("设置", "Settings", "設定")
    }
    pub fn play(self) -> &'static str {
        self.pick("启动", "Play", "起動")
    }
    pub fn back(self) -> &'static str {
        self.pick("返回", "Back", "戻る")
    }
    pub fn refresh(self) -> &'static str {
        self.pick("刷新", "Refresh", "更新")
    }
    pub fn select(self) -> &'static str {
        self.pick("选择", "Select", "選択")
    }
    pub fn exit(self) -> &'static str {
        self.pick("退出", "Exit", "終了")
    }
    pub fn cancel(self) -> &'static str {
        self.pick("取消", "Cancel", "キャンセル")
    }
    pub fn close(self) -> &'static str {
        self.pick("知道了", "OK", "確認")
    }
    pub fn startup(self) -> &'static str {
        self.pick("启动文件", "Startup file", "起動ファイル")
    }
    pub fn cursor(self) -> &'static str {
        self.pick("光标速度", "Pointer speed", "カーソル速度")
    }
    pub fn quality(self, quality: RenderQuality) -> &'static str {
        match quality {
            RenderQuality::Native => self.pick("原生 · 960", "Native · 960", "標準 · 960"),
            RenderQuality::Balanced => self.pick("均衡 · 720", "Balanced · 720", "バランス · 720"),
            RenderQuality::Performance => self.pick("低画质 · 480", "Low · 480", "軽量 · 480"),
        }
    }
    pub fn speed(self, speed: CursorSpeed) -> &'static str {
        match speed {
            CursorSpeed::Slow => self.pick("慢", "Slow", "遅い"),
            CursorSpeed::Normal => self.pick("标准", "Normal", "標準"),
            CursorSpeed::Fast => self.pick("快", "Fast", "速い"),
        }
    }
    pub fn stats(self) -> &'static str {
        self.pick("性能显示", "Performance overlay", "性能表示")
    }
    pub fn scripts(self) -> &'static str {
        self.pick("脚本日志", "Script logs", "スクリプトログ")
    }
    pub fn diagnostics(self) -> &'static str {
        self.pick("引擎诊断", "Engine diagnostics", "エンジン診断")
    }
    pub fn language(self) -> &'static str {
        self.pick("语言", "Language", "言語")
    }
    pub fn theme(self) -> &'static str {
        self.pick("主题", "Theme", "テーマ")
    }
    pub fn theme_name(self, light: bool) -> &'static str {
        if light {
            self.pick("浅色", "Light", "ライト")
        } else {
            self.pick("深色", "Dark", "ダーク")
        }
    }
    pub fn animations(self) -> &'static str {
        self.pick("界面动效", "Interface animations", "画面アニメーション")
    }
    pub fn loading(self) -> &'static str {
        self.pick("正在加载游戏", "Loading game", "ゲームを読み込み中")
    }
    pub fn working(self) -> &'static str {
        self.pick("正在读取，请稍候", "Reading, please wait", "読み込み中です")
    }
    pub fn reset(self) -> &'static str {
        self.pick("使用默认入口", "Use default entry", "標準の起動ファイル")
    }
    pub fn folder(self) -> &'static str {
        self.pick("文件夹", "Folder", "フォルダー")
    }
    pub fn file(self) -> &'static str {
        self.pick("文件", "File", "ファイル")
    }
}
