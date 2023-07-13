use std::{
    fs::{File, OpenOptions},
    os::{
        fd::{AsRawFd, AsFd},
        unix::{io::OwnedFd, fs::OpenOptionsExt}
    },
    path::Path,
    collections::HashMap,
    cmp::min,
    panic::{self, AssertUnwindSafe}
};
use cairo::{ImageSurface, Format, Context, Surface, Rectangle, Antialias};
use rsvg::{Loader, CairoRenderer, SvgHandle};
use drm::control::ClipRect;
use anyhow::Result;
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
use nix::{
    sys::{
        signal::{Signal, SigSet},
        epoll::{Epoll, EpollCreateFlags, EpollEvent, EpollFlags}
    }, 
    errno::Errno
};
use privdrop::PrivDrop;

mod backlight;
mod display;
mod pixel_shift;
mod fonts;
mod config;

use backlight::BacklightManager;
use display::DrmBackend;
use pixel_shift::{PixelShiftManager, PIXEL_SHIFT_WIDTH_PX};
use config::{ButtonConfig, Config};
use crate::config::ConfigManager;

const BUTTON_SPACING_PX: i32 = 16;
const BUTTON_COLOR_INACTIVE: f64 = 0.200;
const BUTTON_COLOR_ACTIVE: f64 = 0.400;
const ICON_SIZE: i32 = 48;
const TIMEOUT_MS: i32 = 10 * 1000;

enum ButtonImage {
    Text(String),
    Svg(SvgHandle),
    Bitmap(ImageSurface)
}

struct Button {
    image: ButtonImage,
    changed: bool,
    active: bool,
    action: Key
}

fn try_load_svg(path: &str) -> Result<ButtonImage> {
    let handle = Loader::new().read_path(format!("/etc/tiny-dfr/{}.svg", path)).or_else(|_| {
        Loader::new().read_path(format!("/usr/share/tiny-dfr/{}.svg", path))
    })?;
    Ok(ButtonImage::Svg(handle))
}

fn try_load_png(path: &str) -> Result<ButtonImage> {
    let mut file = File::open(format!("/etc/tiny-dfr/{}.png", path)).or_else(|_| {
        File::open(format!("/usr/share/tiny-dfr/{}.png", path))
    })?;
    let surf = ImageSurface::create_from_png(&mut file)?;
    if surf.height() == ICON_SIZE && surf.width() == ICON_SIZE {
        return Ok(ButtonImage::Bitmap(surf));
    }
    let resized = ImageSurface::create(Format::ARgb32, ICON_SIZE, ICON_SIZE).unwrap();
    let c = Context::new(&resized).unwrap();
    c.scale(ICON_SIZE as f64 / surf.width() as f64, ICON_SIZE as f64 / surf.height() as f64);
    c.set_source_surface(surf, 0.0, 0.0).unwrap();
    c.set_antialias(Antialias::Best);
    c.paint().unwrap();
    return Ok(ButtonImage::Bitmap(resized));
}

impl Button {
    fn with_config(cfg: ButtonConfig) -> Button {
        if let Some(text) = cfg.text {
            Button::new_text(text, cfg.action)
        } else if let Some(icon) = cfg.icon {
            Button::new_icon(&icon, cfg.action)
        } else {
            panic!("Invalid config, a button must have either Text or Icon")
        }
    }
    fn new_text(text: String, action: Key) -> Button {
        Button {
            action,
            active: false,
            changed: false,
            image: ButtonImage::Text(text)
        }
    }
    fn new_icon(path: &str, action: Key) -> Button {
        let image = try_load_svg(path).or_else(|_| try_load_png(path)).unwrap();
        Button {
            action, image,
            active: false,
            changed: false,
        }
    }
    fn render(&self, c: &Context, height: i32, button_left_edge: f64, button_width: u64, y_shift: f64) {
        match &self.image {
            ButtonImage::Text(text) => {
                let extents = c.text_extents(text).unwrap();
                c.move_to(
                    button_left_edge + (button_width as f64 / 2.0 - extents.width() / 2.0).round(),
                    y_shift + (height as f64 / 2.0 + extents.height() / 2.0).round()
                );
                c.show_text(text).unwrap();
            },
            ButtonImage::Svg(svg) => {
                let renderer = CairoRenderer::new(&svg);
                let x = button_left_edge + (button_width as f64 / 2.0 - (ICON_SIZE / 2) as f64).round();
                let y = y_shift + ((height as f64 - ICON_SIZE as f64) / 2.0).round();

                renderer.render_document(c,
                    &Rectangle::new(x, y, ICON_SIZE as f64, ICON_SIZE as f64)
                ).unwrap();
            }
            ButtonImage::Bitmap(surf) => {
                let x = button_left_edge + (button_width as f64 / 2.0 - (ICON_SIZE / 2) as f64).round();
                let y = y_shift + ((height as f64 - ICON_SIZE as f64) / 2.0).round();
                c.set_source_surface(surf, x, y).unwrap();
                c.rectangle(x, y, ICON_SIZE as f64, ICON_SIZE as f64);
                c.fill().unwrap();
            }
        }
    }
    fn set_active<F>(&mut self, uinput: &mut UInputHandle<F>, active: bool) where F: AsRawFd {
        if self.active != active {
            self.active = active;
            self.changed = true;

            toggle_key(uinput, self.action, active as i32);
        }
    }
}

#[derive(Default)]
pub struct FunctionLayer {
    buttons: Vec<Button>
}

impl FunctionLayer {
    fn with_config(cfg: Vec<ButtonConfig>) -> FunctionLayer {
        if cfg.is_empty() {
            panic!("Invalid configuration, layer has 0 buttons");
        }
        FunctionLayer {
            buttons: cfg.into_iter().map(Button::with_config).collect()
        }
    }
    fn draw(&mut self, config: &Config, width: i32, height: i32, surface: &Surface, pixel_shift: (f64, f64), complete_redraw: bool) -> Vec<ClipRect> {
        let c = Context::new(&surface).unwrap();
        let mut modified_regions = if complete_redraw {
            vec![ClipRect::new(0, 0, height as u16, width as u16)]
        } else {
            Vec::new()
        };
        c.translate(height as f64, 0.0);
        c.rotate((90.0f64).to_radians());
        let pixel_shift_width = if config.enable_pixel_shift { PIXEL_SHIFT_WIDTH_PX } else { 0 };
        let button_width = ((width - pixel_shift_width as i32) - (BUTTON_SPACING_PX * (self.buttons.len() - 1) as i32)) as f64 / self.buttons.len() as f64;
        let radius = 8.0f64;
        let bot = (height as f64) * 0.15;
        let top = (height as f64) * 0.85;
        let (pixel_shift_x, pixel_shift_y) = pixel_shift;

        if complete_redraw {
            c.set_source_rgb(0.0, 0.0, 0.0);
            c.paint().unwrap();
        }
        c.set_font_face(&config.font_face);
        c.set_font_size(32.0);
        for (i, button) in self.buttons.iter_mut().enumerate() {
            if !button.changed && !complete_redraw {
                continue;
            };

            let left_edge = (i as f64 * (button_width + BUTTON_SPACING_PX as f64)).floor() + pixel_shift_x + (pixel_shift_width / 2) as f64;
            let color = if button.active {
                BUTTON_COLOR_ACTIVE
            } else if config.show_button_outlines {
                BUTTON_COLOR_INACTIVE
            } else {
                0.0
            };
            if !complete_redraw {
                c.set_source_rgb(0.0, 0.0, 0.0);
                c.rectangle(left_edge, bot - radius, button_width, top - bot + radius * 2.0);
                c.fill().unwrap();
            }
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
            button.render(&c, height, left_edge, button_width.ceil() as u64, pixel_shift_y);

            button.changed = false;

            if !complete_redraw {
                modified_regions.push(ClipRect::new(
                    height as u16 - top as u16 - radius as u16,
                    left_edge as u16,
                    height as u16 - bot as u16 + radius as u16,
                    left_edge as u16 + button_width as u16
                ));
            }
        }

        modified_regions
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


fn button_hit(num: u32, idx: u32, width: u16, height: u16, x: f64, y: f64) -> bool {
    let button_width = (width as i32 - (BUTTON_SPACING_PX * (num - 1) as i32)) as f64 / num as f64;
    let left_edge = idx as f64 * (button_width + BUTTON_SPACING_PX as f64);
    if x < left_edge || x > (left_edge + button_width) {
        return false
    }
    y > 0.1 * height as f64 && y < 0.9 * height as f64
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

fn main() {
    let mut drm = DrmBackend::open_card().unwrap();
    let (height, width) = drm.mode().size();
    let _ = panic::catch_unwind(AssertUnwindSafe(|| {
        real_main(&mut drm)
    }));
    let crash_bitmap = include_bytes!("crash_bitmap.raw");
    let mut map = drm.map().unwrap();
    let data = map.as_mut();
    let mut wptr = 0;
    for byte in crash_bitmap {
        for i in 0..8 {
            let bit = ((byte >> i) & 0x1) == 0;
            let color = if bit { 0xFF } else { 0x0 };
            data[wptr] = color;
            data[wptr + 1] = color;
            data[wptr + 2] = color;
            data[wptr + 3] = color;
            wptr += 4;
        }
    }
    drop(map);
    drm.dirty(&[ClipRect::new(0, 0, height as u16, width as u16)]).unwrap();
    let mut sigset = SigSet::empty();
    sigset.add(Signal::SIGTERM);
    sigset.wait().unwrap();
}

fn real_main(drm: &mut DrmBackend) {
    let (height, width) = drm.mode().size();
    let (db_width, db_height) = drm.fb_info().unwrap().size();
    let mut uinput = UInputHandle::new(OpenOptions::new().write(true).open("/dev/uinput").unwrap());
    let mut backlight = BacklightManager::new();
    let mut cfg_mgr = ConfigManager::new();
    let (mut cfg, mut layers) = cfg_mgr.load_config();
    let mut pixel_shift = PixelShiftManager::new();

    // drop privileges to input and video group
    let groups = ["input", "video"];

    PrivDrop::default()
        .user("nobody")
        .group_list(&groups)
        .apply()
        .unwrap_or_else(|e| { panic!("Failed to drop privileges: {}", e) });

    let mut surface = ImageSurface::create(Format::ARgb32, db_width as i32, db_height as i32).unwrap();
    let mut active_layer = 0;
    let mut needs_complete_redraw = true;

    let mut input_tb = Libinput::new_with_udev(Interface);
    let mut input_main = Libinput::new_with_udev(Interface);
    input_tb.udev_assign_seat("seat-touchbar").unwrap();
    input_main.udev_assign_seat("seat0").unwrap();
    let epoll = Epoll::new(EpollCreateFlags::empty()).unwrap();
    epoll.add(input_main.as_fd(), EpollEvent::new(EpollFlags::EPOLLIN, 0)).unwrap();
    epoll.add(input_tb.as_fd(), EpollEvent::new(EpollFlags::EPOLLIN, 1)).unwrap();
    epoll.add(cfg_mgr.fd(), EpollEvent::new(EpollFlags::EPOLLIN, 2)).unwrap();
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
        if cfg_mgr.update_config(&mut cfg, &mut layers) {
            active_layer = 0;
            needs_complete_redraw = true;
        }

        let mut next_timeout_ms = TIMEOUT_MS;
        if cfg.enable_pixel_shift {
            let (pixel_shift_needs_redraw, pixel_shift_next_timeout_ms) = pixel_shift.update();
            if pixel_shift_needs_redraw {
                needs_complete_redraw = true;
            }
            next_timeout_ms = min(next_timeout_ms, pixel_shift_next_timeout_ms);
        }

        if needs_complete_redraw || layers[active_layer].buttons.iter().any(|b| b.changed) {
            let shift = if cfg.enable_pixel_shift {
                pixel_shift.get()
            } else {
                (0.0, 0.0)
            };
            let clips = layers[active_layer].draw(&cfg, width as i32, height as i32, &surface, shift, needs_complete_redraw);
            let data = surface.data().unwrap();
            drm.map().unwrap().as_mut()[..data.len()].copy_from_slice(&data);
            drm.dirty(&clips).unwrap();
            needs_complete_redraw = false;
        }

        match epoll.wait(&mut [EpollEvent::new(EpollFlags::EPOLLIN, 0)], next_timeout_ms as isize) {
            Err(Errno::EINTR) | Ok(_) => { 0 },
            e => e.unwrap(),
        };
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
                            needs_complete_redraw = true;
                        }
                    }
                },
                Event::Touch(te) => {
                    if Some(te.device()) != digitizer || backlight.current_bl() == 0 {
                        continue
                    }
                    match te {
                        TouchEvent::Down(dn) => {
                            let x = dn.x_transformed(width as u32);
                            let y = dn.y_transformed(height as u32);
                            let btn = (x / (width as f64 / layers[active_layer].buttons.len() as f64)) as u32;
                            if button_hit(layers[active_layer].buttons.len() as u32, btn, width, height, x, y) {
                                touches.insert(dn.seat_slot(), (active_layer, btn));
                                layers[active_layer].buttons[btn as usize].set_active(&mut uinput, true);
                            }
                        },
                        TouchEvent::Motion(mtn) => {
                            if !touches.contains_key(&mtn.seat_slot()) {
                                continue;
                            }

                            let x = mtn.x_transformed(width as u32);
                            let y = mtn.y_transformed(height as u32);
                            let (layer, btn) = *touches.get(&mtn.seat_slot()).unwrap();
                            let hit = button_hit(layers[layer].buttons.len() as u32, btn, width, height, x, y);
                            layers[layer].buttons[btn as usize].set_active(&mut uinput, hit);
                        },
                        TouchEvent::Up(up) => {
                            if !touches.contains_key(&up.seat_slot()) {
                                continue;
                            }
                            let (layer, btn) = *touches.get(&up.seat_slot()).unwrap();
                            layers[layer].buttons[btn as usize].set_active(&mut uinput, false);
                        }
                        _ => {}
                    }
                },
                _ => {}
            }
        }
        backlight.update_backlight(&cfg);
    }
}
