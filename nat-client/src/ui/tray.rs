//! 系统托盘模块
//!
//! 使用 tray-icon 0.24 实现跨平台系统托盘。
//! 托盘图标颜色反映当前连接状态（绿=在线，红=离线）。
//!
//! ## 平台说明
//! - **Windows**：使用 Win32 Shell 通知区图标
//! - **macOS**：使用 NSStatusBar
//! - **Linux**：需要 libayatana-appindicator3 或 libappindicator3
//!   安装：`sudo apt install libayatana-appindicator3-dev`

use core_common::log;
use tray_icon::{
    menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem},
    Icon, TrayIcon, TrayIconBuilder, TrayIconEvent,
};

// ──────────────────────────────────────────────────────────────────────────────
// 菜单项 ID（全局，用于事件匹配）
// ──────────────────────────────────────────────────────────────────────────────

/// 托盘菜单事件类型
#[derive(Debug, Clone, PartialEq)]
pub enum TrayAction {
    /// 显示/隐藏主窗口
    ToggleWindow,
    /// 打开首页
    GoHome,
    /// 打开连接页
    GoConnect,
    /// 打开账户页
    GoAccount,
    /// 退出程序
    Quit,
}

/// 待处理的托盘动作（由主事件循环处理）
pub struct TrayManager {
    _tray: TrayIcon,
    show_hide_item_id: tray_icon::menu::MenuId,
    home_item_id: tray_icon::menu::MenuId,
    connect_item_id: tray_icon::menu::MenuId,
    account_item_id: tray_icon::menu::MenuId,
    quit_item_id: tray_icon::menu::MenuId,
    online: bool,
}

impl TrayManager {
    /// 创建系统托盘图标和菜单
    pub fn new() -> Result<Self, Box<dyn std::error::Error>> {
        let icon = make_icon(false);

        // 构建菜单
        let show_hide_item = MenuItem::new("显示/隐藏窗口", true, None);
        let home_item = MenuItem::new("首页", true, None);
        let connect_item = MenuItem::new("连接对端…", true, None);
        let account_item = MenuItem::new("账户", true, None);
        let quit_item = MenuItem::new("退出", true, None);

        let show_hide_id = show_hide_item.id().clone();
        let home_id = home_item.id().clone();
        let connect_id = connect_item.id().clone();
        let account_id = account_item.id().clone();
        let quit_id = quit_item.id().clone();

        let menu = Menu::new();
        menu.append_items(&[
            &show_hide_item,
            &PredefinedMenuItem::separator(),
            &home_item,
            &connect_item,
            &account_item,
            &PredefinedMenuItem::separator(),
            &quit_item,
        ])?;

        let tray = TrayIconBuilder::new()
            .with_tooltip("NAT Client — 离线")
            .with_icon(icon)
            .with_menu(Box::new(menu))
            .build()?;

        Ok(Self {
            _tray: tray,
            show_hide_item_id: show_hide_id,
            home_item_id: home_id,
            connect_item_id: connect_id,
            account_item_id: account_id,
            quit_item_id: quit_id,
            online: false,
        })
    }

    /// 更新托盘图标和 tooltip（根据在线状态）
    pub fn set_online(&mut self, online: bool) {
        if self.online == online {
            return;
        }
        self.online = online;
        let icon = make_icon(online);
        let tooltip = if online {
            "NAT Client — 已上线"
        } else {
            "NAT Client — 离线"
        };
        let _ = self._tray.set_icon(Some(icon));
        let _ = self._tray.set_tooltip(Some(tooltip));
    }

    /// 轮询一次托盘事件（在 Slint timer 中调用，每 50ms 一次）
    ///
    /// 返回 `Some(TrayAction)` 表示有动作需要处理。
    pub fn poll(&self) -> Option<TrayAction> {
        // 先检查图标点击
        if let Ok(event) = TrayIconEvent::receiver().try_recv() {
            use tray_icon::{MouseButton, MouseButtonState, TrayIconEvent as E};
            if let E::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                return Some(TrayAction::ToggleWindow);
            }
        }

        // 再检查菜单事件
        if let Ok(event) = MenuEvent::receiver().try_recv() {
            let id = event.id();
            if id == &self.show_hide_item_id {
                return Some(TrayAction::ToggleWindow);
            }
            if id == &self.home_item_id {
                return Some(TrayAction::GoHome);
            }
            if id == &self.connect_item_id {
                return Some(TrayAction::GoConnect);
            }
            if id == &self.account_item_id {
                return Some(TrayAction::GoAccount);
            }
            if id == &self.quit_item_id {
                return Some(TrayAction::Quit);
            }
        }

        None
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// 图标加载（从嵌入的 ICO 文件解码，并叠加在线/离线状态指示圆点）
// ──────────────────────────────────────────────────────────────────────────────

/// 应用 ICO 文件字节（编译时嵌入）
static ICON_BYTES: &[u8] = include_bytes!("../../icons/logo.ico");

/// 加载 logo.ico 并缩放到 32×32，叠加状态圆点后返回托盘 Icon
///
/// - online = true  → 右下角绿色圆点
/// - online = false → 右下角灰色圆点
fn make_icon(online: bool) -> Icon {
    const SIZE: u32 = 32;

    let rgba_pixels = load_logo_rgba(SIZE).unwrap_or_else(|| make_fallback_rgba(SIZE, online));

    let mut pixels = rgba_pixels;
    stamp_status_dot(&mut pixels, SIZE, online);
    Icon::from_rgba(pixels, SIZE, SIZE).expect("托盘图标创建失败")
}

/// 从 ICON_BYTES 解码 ICO，缩放到 target_size，返回 RGBA 字节。
/// 失败时返回 None。
fn load_logo_rgba(target_size: u32) -> Option<Vec<u8>> {
    use image::imageops::FilterType;
    let img = image::load_from_memory_with_format(ICON_BYTES, image::ImageFormat::Ico).ok()?;
    let img = img.resize_exact(target_size, target_size, FilterType::Lanczos3);
    Some(img.to_rgba8().into_raw())
}

/// 在右下角叠加 7×7 状态圆点（绿=在线，灰=离线）
fn stamp_status_dot(pixels: &mut Vec<u8>, size: u32, online: bool) {
    let dot_color: [u8; 4] = if online {
        [0x30, 0xd1, 0x58, 255] // 绿
    } else {
        [0x8e, 0x8e, 0x93, 200] // 灰
    };
    const DOT_R: f32 = 3.5;
    let cx = size as f32 - DOT_R - 1.0;
    let cy = size as f32 - DOT_R - 1.0;

    for y in 0..size {
        for x in 0..size {
            let dx = x as f32 - cx;
            let dy = y as f32 - cy;
            if (dx * dx + dy * dy).sqrt() <= DOT_R {
                let idx = ((y * size + x) * 4) as usize;
                if idx + 3 < pixels.len() {
                    pixels[idx] = dot_color[0];
                    pixels[idx + 1] = dot_color[1];
                    pixels[idx + 2] = dot_color[2];
                    pixels[idx + 3] = dot_color[3];
                }
            }
        }
    }
}

/// ICO 解码失败时的纯色兜底图标
fn make_fallback_rgba(size: u32, online: bool) -> Vec<u8> {
    let fill: [u8; 4] = if online {
        [0x4c, 0x6e, 0xf5, 255]
    } else {
        [0x86, 0x8e, 0x96, 255]
    };
    fill.iter().cloned().cycle().take((size * size * 4) as usize).collect()
}
