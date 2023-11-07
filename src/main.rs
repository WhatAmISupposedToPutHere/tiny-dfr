use std::{
    fs::{File, OpenOptions, read_to_string},
    os::{
        fd::AsRawFd,
        unix::{io::OwnedFd, fs::OpenOptionsExt}
    },
    path::Path,
    collections::HashMap,
    cmp::min
};
use std::os::fd::AsFd;
use cairo::{
    ImageSurface, Format, Context, Surface,
    FontSlant, FontWeight, Rectangle
};
use rsvg::{Loader, CairoRenderer, SvgHandle};
use drm::control::ClipRect;
use anyhow::{Error, Result};
use input::{
    Libinput, LibinputInterface, Device as InputDevice,
    event::{
        Event, device::DeviceEvent, EventTrait,
        touch::{TouchEvent, TouchEventPosition, TouchEventSlot},
        keyboard::{KeyboardEvent, KeyboardEventTrait, KeyState}
    }
};
use libc::{O_ACCMODE, O_RDONLY, O_RDWR, O_WRONLY, c_char};
use input_linux::{uinput::UInputHandle, EventKind, Key, SynchronizeKind};
use input_linux_sys::{uinput_setup, input_id, timeval, input_event};
use nix::poll::{poll, PollFd, PollFlags};
use privdrop::PrivDrop;
use serde::Deserialize;

mod backlight;
mod display;
mod pixel_shift;

use backlight::BacklightManager;
use display::DrmBackend;
use pixel_shift::PixelShiftManager;
use pixel_shift::PIXEL_SHIFT_WIDTH_PX;

const DFR_WIDTH: i32 = 2008;
const DFR_HEIGHT: i32 = 64;
const BUTTON_SPACING_PX: i32 = 16;
const BUTTON_COLOR_INACTIVE: f64 = 0.200;
const BUTTON_COLOR_ACTIVE: f64 = 0.400;
const ICON_SIZE: i32 = 48;
const TIMEOUT_MS: i32 = 10 * 1000;

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct ConfigProxy {
    media_layer_default: Option<bool>,
    show_button_outlines: Option<bool>,
    enable_pixel_shift: Option<bool>
}

struct Config {
    media_layer_default: bool,
    show_button_outlines: bool,
    enable_pixel_shift: bool
}

enum ButtonImage {
    Text(&'static str),
    Svg(SvgHandle)
}

struct Button {
    image: ButtonImage,
    action: Key
}

impl Button {
    fn new_text(text: &'static str, action: Key) -> Button {
        Button {
            action, image: ButtonImage::Text(text)
        }
    }
    fn new_svg(path: &'static str, action: Key) -> Button {
        let svg = Loader::new().read_path(format!("/usr/share/tiny-dfr/{}.svg", path)).unwrap();
        Button {
            action, image: ButtonImage::Svg(svg)
        }
    }
    fn render(&self, c: &Context, button_left_edge: f64, button_width: u64, y_shift: f64) {
        match &self.image {
            ButtonImage::Text(text) => {
                let extents = c.text_extents(text).unwrap();
                c.move_to(
                    button_left_edge + (button_width as f64 / 2.0 - extents.width() / 2.0).round(),
                    y_shift + (DFR_HEIGHT as f64 / 2.0 + extents.height() / 2.0).round()
                );
                c.show_text(text).unwrap();
            },
            ButtonImage::Svg(svg) => {
                let renderer = CairoRenderer::new(&svg);
                let x = button_left_edge + (button_width as f64 / 2.0 - (ICON_SIZE / 2) as f64).round();
                let y = y_shift + ((DFR_HEIGHT as f64 - ICON_SIZE as f64) / 2.0).round();

                renderer.render_document(c,
                    &Rectangle::new(x, y, ICON_SIZE as f64, ICON_SIZE as f64)
                ).unwrap();
            }
        }
    }
}

struct FunctionLayer {
    buttons: Vec<Button>
}

impl FunctionLayer {
    fn draw(&self, config: &Config, surface: &Surface, active_buttons: &[bool], pixel_shift: (f64, f64)) {
        let c = Context::new(&surface).unwrap();
        c.translate(DFR_HEIGHT as f64, 0.0);
        c.rotate((90.0f64).to_radians());
        let button_width = ((DFR_WIDTH - PIXEL_SHIFT_WIDTH_PX as i32) - (BUTTON_SPACING_PX * (self.buttons.len() - 1) as i32)) as f64 / self.buttons.len() as f64;
        let radius = 8.0f64;
        let bot = (DFR_HEIGHT as f64) * 0.2;
        let top = (DFR_HEIGHT as f64) * 0.85;
        let (pixel_shift_x, pixel_shift_y) = pixel_shift;

        c.set_source_rgb(0.0, 0.0, 0.0);
        c.paint().unwrap();
        c.select_font_face("sans-serif", FontSlant::Normal, FontWeight::Bold);
        c.set_font_size(32.0);
        for (i, button) in self.buttons.iter().enumerate() {
            let left_edge = (i as f64 * (button_width + BUTTON_SPACING_PX as f64)).floor() + pixel_shift_x + (PIXEL_SHIFT_WIDTH_PX / 2) as f64;
            let color = if active_buttons[i] {
                BUTTON_COLOR_ACTIVE
            } else if config.show_button_outlines {
                BUTTON_COLOR_INACTIVE
            } else {
                0.0
            };
            c.set_source_rgb(color, color, color);
            // draw box with rounded corners
            c.new_sub_path();
            let left = left_edge + radius;
            let right = (left_edge + button_width.ceil()) - radius;
            c.arc(
                right,
                bot,
                radius,
                (-90.0f64).to_radians(),
                (0.0f64).to_radians(),
            );
            c.arc(
                right,
                top,
                radius,
                (0.0f64).to_radians(),
                (90.0f64).to_radians(),
            );
            c.arc(
                left,
                top,
                radius,
                (90.0f64).to_radians(),
                (180.0f64).to_radians(),
            );
            c.arc(
                left,
                bot,
                radius,
                (180.0f64).to_radians(),
                (270.0f64).to_radians(),
            );
            c.close_path();

            c.fill().unwrap();
            c.set_source_rgb(1.0, 1.0, 1.0);
            button.render(&c, left_edge, button_width.ceil() as u64, pixel_shift_y);
        }
    }
}

struct Interface;

impl LibinputInterface for Interface {
    fn open_restricted(&mut self, path: &Path, flags: i32) -> Result<OwnedFd, i32> {
        let mode = flags & O_ACCMODE;

        OpenOptions::new()
            .custom_flags(flags)
            .read(mode == O_RDONLY || mode == O_RDWR)
            .write(mode == O_WRONLY || mode == O_RDWR)
            .open(path)
            .map(|file| file.into())
            .map_err(|err| err.raw_os_error().unwrap())
    }
    fn close_restricted(&mut self, fd: OwnedFd) {
        _ = File::from(fd);
    }
}


fn button_hit(num: u32, idx: u32, x: f64, y: f64) -> bool {
    let button_width = (DFR_WIDTH - (BUTTON_SPACING_PX * (num - 1) as i32)) as f64 / num as f64;
    let left_edge = idx as f64 * (button_width + BUTTON_SPACING_PX as f64);
    if x < left_edge || x > (left_edge + button_width) {
        return false
    }
    y > 0.1 * DFR_HEIGHT as f64 && y < 0.9 * DFR_HEIGHT as f64
}

fn emit<F>(uinput: &mut UInputHandle<F>, ty: EventKind, code: u16, value: i32) where F: AsRawFd {
    uinput.write(&[input_event {
        value: value,
        type_: ty as u16,
        code: code,
        time: timeval {
            tv_sec: 0,
            tv_usec: 0
        }
    }]).unwrap();
}

fn toggle_key<F>(uinput: &mut UInputHandle<F>, code: Key, value: i32) where F: AsRawFd {
    emit(uinput, EventKind::Key, code as u16, value);
    emit(uinput, EventKind::Synchronize, SynchronizeKind::Report as u16, 0);
}

fn load_config() -> Config {
    let mut base = toml::from_str::<ConfigProxy>(&read_to_string("/usr/share/tiny-dfr/config.toml").unwrap()).unwrap();
    let user = read_to_string("/etc/tiny-dfr/config.toml").map_err::<Error, _>(|e| e.into())
        .and_then(|r| Ok(toml::from_str::<ConfigProxy>(&r)?));
    if let Ok(user) = user {
        base.media_layer_default = user.media_layer_default.or(base.media_layer_default);
        base.show_button_outlines = user.show_button_outlines.or(base.show_button_outlines);
        base.enable_pixel_shift = user.enable_pixel_shift.or(base.enable_pixel_shift);
    };
    Config {
        media_layer_default: base.media_layer_default.unwrap(),
        show_button_outlines: base.show_button_outlines.unwrap(),
        enable_pixel_shift: base.enable_pixel_shift.unwrap(),
    }
}

fn main() {
    let mut uinput = UInputHandle::new(OpenOptions::new().write(true).open("/dev/uinput").unwrap());
    let mut backlight = BacklightManager::new();
    let config = load_config();
    let mut pixel_shift = PixelShiftManager::new();

    // drop privileges to input and video group
    let groups = ["input", "video"];

    PrivDrop::default()
        .user("nobody")
        .group_list(&groups)
        .apply()
        .unwrap_or_else(|e| { panic!("Failed to drop privileges: {}", e) });

    let mut surface = ImageSurface::create(Format::ARgb32, DFR_HEIGHT, DFR_WIDTH).unwrap();
    let mut active_layer = 0;
    let fkey_layer = FunctionLayer {
        buttons: vec![
            Button::new_text("F1", Key::F1),
            Button::new_text("F2", Key::F2),
            Button::new_text("F3", Key::F3),
            Button::new_text("F4", Key::F4),
            Button::new_text("F5", Key::F5),
            Button::new_text("F6", Key::F6),
            Button::new_text("F7", Key::F7),
            Button::new_text("F8", Key::F8),
            Button::new_text("F9", Key::F9),
            Button::new_text("F10", Key::F10),
            Button::new_text("F11", Key::F11),
            Button::new_text("F12", Key::F12)
        ]
    };
    let media_layer = FunctionLayer {
        buttons: vec![
            Button::new_svg("brightness_low", Key::BrightnessDown),
            Button::new_svg("brightness_high", Key::BrightnessUp),
            Button::new_svg("mic_off", Key::MicMute),
            Button::new_svg("search", Key::Search),
            Button::new_svg("backlight_low", Key::IllumDown),
            Button::new_svg("backlight_high", Key::IllumUp),
            Button::new_svg("fast_rewind", Key::PreviousSong),
            Button::new_svg("play_pause", Key::PlayPause),
            Button::new_svg("fast_forward", Key::NextSong),
            Button::new_svg("volume_off", Key::Mute),
            Button::new_svg("volume_down", Key::VolumeDown),
            Button::new_svg("volume_up", Key::VolumeUp)
        ]
    };
    let layers = if config.media_layer_default { [media_layer, fkey_layer] } else { [fkey_layer, media_layer] };
    let mut button_states = [vec![false; 12], vec![false; 12]];
    let mut needs_redraw = true;
    let mut drm = DrmBackend::open_card().unwrap();
    let mut input_tb = Libinput::new_with_udev(Interface);
    let mut input_main = Libinput::new_with_udev(Interface);
    input_tb.udev_assign_seat("seat-touchbar").unwrap();
    input_main.udev_assign_seat("seat0").unwrap();
    let fd_tb = input_tb.as_fd().try_clone_to_owned().unwrap();
    let fd_main = input_main.as_fd().try_clone_to_owned().unwrap();
    let pollfd_tb = PollFd::new(&fd_tb, PollFlags::POLLIN);
    let pollfd_main = PollFd::new(&fd_main, PollFlags::POLLIN);
    uinput.set_evbit(EventKind::Key).unwrap();
    for layer in &layers {
        for button in &layer.buttons {
            uinput.set_keybit(button.action).unwrap();
        }
    }
    let mut dev_name_c = [0 as c_char; 80];
    let dev_name = "Dynamic Function Row Virtual Input Device".as_bytes();
    for i in 0..dev_name.len() {
        dev_name_c[i] = dev_name[i] as c_char;
    }
    uinput.dev_setup(&uinput_setup {
        id: input_id {
            bustype: 0x19,
            vendor: 0x1209,
            product: 0x316E,
            version: 1
        },
        ff_effects_max: 0,
        name: dev_name_c
    }).unwrap();
    uinput.dev_create().unwrap();

    let mut digitizer: Option<InputDevice> = None;
    let mut touches = HashMap::new();
    loop {
        let mut next_timeout_ms = TIMEOUT_MS;

        if config.enable_pixel_shift {
            let (pixel_shift_needs_redraw, pixel_shift_next_timeout_ms) = pixel_shift.update();
            if pixel_shift_needs_redraw {
                needs_redraw = true;
            }
            next_timeout_ms = min(next_timeout_ms, pixel_shift_next_timeout_ms);
        }


        if needs_redraw {
            needs_redraw = false;
            let shift = if config.enable_pixel_shift {
                pixel_shift.get()
            } else {
                (0.0, 0.0)
            };
            layers[active_layer].draw(&config, &surface, &button_states[active_layer], shift);
            let data = surface.data().unwrap();
            drm.map().unwrap().as_mut()[..data.len()].copy_from_slice(&data);
            drm.dirty(&[ClipRect::new(0, 0, DFR_HEIGHT as u16, DFR_WIDTH as u16)]).unwrap();
        }

        poll(&mut [pollfd_tb, pollfd_main], next_timeout_ms).unwrap();
        input_tb.dispatch().unwrap();
        input_main.dispatch().unwrap();
        for event in &mut input_tb.clone().chain(input_main.clone()) {
            backlight.process_event(&event);
            match event {
                Event::Device(DeviceEvent::Added(evt)) => {
                    let dev = evt.device();
                    if dev.name().contains(" Touch Bar") {
                        digitizer = Some(dev);
                    }
                },
                Event::Keyboard(KeyboardEvent::Key(key)) => {
                    if key.key() == Key::Fn as u32 {
                        let new_layer = match key.key_state() {
                            KeyState::Pressed => 1,
                            KeyState::Released => 0
                        };
                        if active_layer != new_layer {
                            active_layer = new_layer;
                            needs_redraw = true;
                        }
                    }
                },
                Event::Touch(te) => {
                    if Some(te.device()) != digitizer || backlight.current_bl() == 0 {
                        continue
                    }
                    match te {
                        TouchEvent::Down(dn) => {
                            let x = dn.x_transformed(DFR_WIDTH as u32);
                            let y = dn.y_transformed(DFR_HEIGHT as u32);
                            let btn = (x / (DFR_WIDTH as f64 / layers[active_layer].buttons.len() as f64)) as u32;
                            if button_hit(layers[active_layer].buttons.len() as u32, btn, x, y) {
                                touches.insert(dn.seat_slot(), (active_layer, btn));
                                button_states[active_layer][btn as usize] = true;
                                needs_redraw = true;
                                toggle_key(&mut uinput, layers[active_layer].buttons[btn as usize].action, 1);
                            }
                        },
                        TouchEvent::Motion(mtn) => {
                            if !touches.contains_key(&mtn.seat_slot()) {
                                continue;
                            }

                            let x = mtn.x_transformed(DFR_WIDTH as u32);
                            let y = mtn.y_transformed(DFR_HEIGHT as u32);
                            let (layer, btn) = *touches.get(&mtn.seat_slot()).unwrap();
                            let hit = button_hit(layers[layer].buttons.len() as u32, btn, x, y);
                            if button_states[layer][btn as usize] != hit {
                                button_states[layer][btn as usize] = hit;
                                needs_redraw = true;
                                toggle_key(&mut uinput, layers[active_layer].buttons[btn as usize].action, hit as i32);
                            }
                        },
                        TouchEvent::Up(up) => {
                            if !touches.contains_key(&up.seat_slot()) {
                                continue;
                            }
                            let (layer, btn) = *touches.get(&up.seat_slot()).unwrap();
                            if button_states[layer][btn as usize] {
                                button_states[layer][btn as usize] = false;
                                needs_redraw = true;
                                toggle_key(&mut uinput, layers[active_layer].buttons[btn as usize].action, 0);
                            }
                        }
                        _ => {}
                    }
                },
                _ => {}
            }
        }
        backlight.update_backlight();
    }
}
