use std::{
    fs::read_to_string,
    os::fd::AsFd
};
use anyhow::Error;
use cairo::FontFace;
use csscolorparser::Color;
use crate::FunctionLayer;
use crate::fonts::{FontConfig, Pattern};
use freetype::Library as FtLibrary;
use input_linux::Key;
use nix::{
    errno::Errno,
    sys::inotify::{AddWatchFlags, InitFlags, Inotify, WatchDescriptor}
};
use serde::Deserialize;

const USER_CFG_PATH: &'static str = "/etc/tiny-dfr/config.toml";
static DEFAULT_COLORS: ColorConfig = ColorConfig {
    text: Color {
        r: 1.0,
        g: 1.0,
        b: 1.0,
        a: 1.0,
    },
    button_inactive: Color {
        r: 0.2,
        g: 0.2,
        b: 0.2,
        a: 1.0,
    },
    button_active: Color {
        r: 0.4,
        g: 0.4,
        b: 0.4,
        a: 1.0,
    },
    background: Color {
        r: 0.0,
        g: 0.0,
        b: 0.0,
        a: 1.0,
    },
};

pub struct Config {
    pub show_button_outlines: bool,
    pub enable_pixel_shift: bool,
    pub font_face: FontFace,
    pub adaptive_brightness: bool,
    pub colors: ColorConfig,
}

#[derive(Clone)]
pub struct ColorConfig {
    pub text: Color,
    pub button_inactive: Color,
    pub button_active: Color,
    pub background: Color,
}

impl Default for ColorConfig {
    fn default() -> Self {
        DEFAULT_COLORS.clone()
    }
}

impl From<ColorConfigProxy> for ColorConfig {
    fn from(
        ColorConfigProxy {
            text,
            button_inactive,
            button_active,
            background,
        }: ColorConfigProxy,
    ) -> Self {
        Self {
            text: text.unwrap_or(DEFAULT_COLORS.text.clone()),
            button_inactive: button_inactive.unwrap_or(DEFAULT_COLORS.button_inactive.clone()),
            button_active: button_active.unwrap_or(DEFAULT_COLORS.button_active.clone()),
            background: background.unwrap_or(DEFAULT_COLORS.background.clone())
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct ConfigProxy {
    media_layer_default: Option<bool>,
    show_button_outlines: Option<bool>,
    enable_pixel_shift: Option<bool>,
    font_template: Option<String>,
    adaptive_brightness: Option<bool>,
    primary_layer_keys: Option<Vec<ButtonConfig>>,
    media_layer_keys: Option<Vec<ButtonConfig>>,
    colors: Option<ColorConfigProxy>
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct ColorConfigProxy {
    text: Option<Color>,
    button_inactive: Option<Color>,
    button_active: Option<Color>,
    background: Option<Color>,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct ButtonConfig {
    #[serde(alias = "Svg")]
    pub icon: Option<String>,
    pub text: Option<String>,
    pub action: Key
}

fn load_font(name: &str) -> FontFace {
    let fontconfig = FontConfig::new();
    let mut pattern = Pattern::new(name);
    fontconfig.perform_substitutions(&mut pattern);
    let pat_match = fontconfig.match_pattern(&pattern);
    let file_name = pat_match.get_file_name();
    let file_idx = pat_match.get_font_index();
    let ft_library = FtLibrary::init().unwrap();
    let face = ft_library.new_face(file_name, file_idx).unwrap();
    FontFace::create_from_ft(&face).unwrap()
}

fn load_config() -> (Config, [FunctionLayer; 2]) {
    let mut base = toml::from_str::<ConfigProxy>(&read_to_string("/usr/share/tiny-dfr/config.toml").unwrap()).unwrap();
    let user = read_to_string(USER_CFG_PATH).map_err::<Error, _>(|e| e.into())
        .and_then(|r| Ok(toml::from_str::<ConfigProxy>(&r)?));
    if let Ok(user) = user {
        base.media_layer_default = user.media_layer_default.or(base.media_layer_default);
        base.show_button_outlines = user.show_button_outlines.or(base.show_button_outlines);
        base.enable_pixel_shift = user.enable_pixel_shift.or(base.enable_pixel_shift);
        base.font_template = user.font_template.or(base.font_template);
        base.adaptive_brightness = user.adaptive_brightness.or(base.adaptive_brightness);
        base.media_layer_keys = user.media_layer_keys.or(base.media_layer_keys);
        base.primary_layer_keys = user.primary_layer_keys.or(base.primary_layer_keys);
        base.colors = user.colors.or(base.colors)
    };
    let media_layer = FunctionLayer::with_config(base.media_layer_keys.unwrap());
    let fkey_layer = FunctionLayer::with_config(base.primary_layer_keys.unwrap());
    let layers = if base.media_layer_default.unwrap(){ [media_layer, fkey_layer] } else { [fkey_layer, media_layer] };
    let cfg = Config {
        show_button_outlines: base.show_button_outlines.unwrap(),
        enable_pixel_shift: base.enable_pixel_shift.unwrap(),
        adaptive_brightness: base.adaptive_brightness.unwrap(),
        font_face: load_font(&base.font_template.unwrap()),
        colors: base.colors.map(Into::into).unwrap_or_default(),
    };
    (cfg, layers)
}

pub struct ConfigManager {
    inotify_fd: Inotify,
    watch_desc: Option<WatchDescriptor>
}

fn arm_inotify(inotify_fd: &Inotify) -> Option<WatchDescriptor> {
    let flags = AddWatchFlags::IN_MOVED_TO | AddWatchFlags::IN_CLOSE | AddWatchFlags::IN_ONESHOT;
    match inotify_fd.add_watch(USER_CFG_PATH, flags) {
        Ok(wd) => Some(wd),
        Err(Errno::ENOENT) => None,
        e => Some(e.unwrap())
    }
}

impl ConfigManager {
    pub fn new() -> ConfigManager {
        let inotify_fd = Inotify::init(InitFlags::IN_NONBLOCK).unwrap();
        let watch_desc = arm_inotify(&inotify_fd);
        ConfigManager {
            inotify_fd, watch_desc
        }
    }
    pub fn load_config(&self) -> (Config, [FunctionLayer; 2]) {
        load_config()
    }
    pub fn update_config(&mut self, cfg: &mut Config, layers: &mut [FunctionLayer; 2]) -> bool {
        if self.watch_desc.is_none() {
            self.watch_desc = arm_inotify(&self.inotify_fd);
            return false;
        }
        let evts = match self.inotify_fd.read_events() {
            Ok(e) => e,
            Err(Errno::EAGAIN) => Vec::new(),
            r => r.unwrap(),
        };
        let mut ret = false;
        for evt in evts {
            if evt.wd != self.watch_desc.unwrap() {
                continue
            }
            let parts = load_config();
            *cfg = parts.0;
            *layers = parts.1;
            ret = true;
            self.watch_desc = arm_inotify(&self.inotify_fd);
        }
        ret
    }
    pub fn fd(&self) -> &impl AsFd {
        &self.inotify_fd
    }
}
