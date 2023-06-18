use std::{
    fs::{File, OpenOptions},
    os::{
        fd::AsRawFd,
        unix::{io::OwnedFd, fs::OpenOptionsExt}
    },
    path::Path,
    collections::HashMap,
};
use cairo::{
    ImageSurface, Format, Context, Surface,
    FontSlant, FontWeight
};
use drm::control::ClipRect;
use anyhow::Result;
use input::{
    Libinput, LibinputInterface, Device as InputDevice,
    event::{
        Event, device::DeviceEvent, EventTrait,
        touch::{TouchEvent, TouchEventPosition, TouchEventSlot}
    }
};
use libc::{O_RDONLY, O_RDWR, O_WRONLY};
use input_linux::{uinput::UInputHandle, EventKind, Key, SynchronizeKind};
use input_linux_sys::{uinput_setup, input_id, timeval, input_event};
use nix::poll::{poll, PollFd, PollFlags};
use privdrop::PrivDrop;

mod backlight;
mod display;

use backlight::BacklightManager;
use display::DrmBackend;

const DFR_WIDTH: i32 = 2008;
const DFR_HEIGHT: i32 = 64;
const BUTTON_COLOR_INACTIVE: f64 = 0.267;
const BUTTON_COLOR_ACTIVE: f64 = 0.567;
const TIMEOUT_MS: i32 = 30 * 1000;

struct Button {
    text: String,
    action: Key
}

struct FunctionLayer {
    buttons: Vec<Button>
}

impl FunctionLayer {
    fn draw(&self, surface: &Surface, active_buttons: &[bool]) {
        let c = Context::new(&surface).unwrap();
        c.translate(DFR_HEIGHT as f64, 0.0);
        c.rotate((90.0f64).to_radians());
        let button_width = DFR_WIDTH as f64 / (self.buttons.len() + 1) as f64;
        let spacing_width = (DFR_WIDTH as f64 - self.buttons.len() as f64 * button_width) / (self.buttons.len() + 1) as f64;
        let radius = 8.0f64;
        let bot = 0.09 * DFR_HEIGHT as f64 + radius;
        let top = bot + 0.82 * DFR_HEIGHT as f64 - 2.0 * radius;
        c.set_source_rgb(0.0, 0.0, 0.0);
        c.paint().unwrap();
        c.select_font_face("sans-serif", FontSlant::Normal, FontWeight::Normal);
        c.set_font_size(32.0);
        for (i, button) in self.buttons.iter().enumerate() {
            let left_edge = i as f64 * (button_width + spacing_width) + spacing_width;
            let color = if active_buttons[i] { BUTTON_COLOR_ACTIVE } else { BUTTON_COLOR_INACTIVE };
            c.set_source_rgb(color, color, color);
            // draw box with rounded corners
            c.new_sub_path();
            let left = left_edge + radius;
            let right = left_edge + button_width - radius;
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
            let extents = c.text_extents(&button.text).unwrap();
            c.move_to(
                left_edge + button_width / 2.0 - extents.width() / 2.0,
                DFR_HEIGHT as f64 / 2.0 + extents.height() / 2.0
            );
            c.show_text(&button.text).unwrap();
        }
    }
}

struct Interface;

impl LibinputInterface for Interface {
    fn open_restricted(&mut self, path: &Path, flags: i32) -> Result<OwnedFd, i32> {
        OpenOptions::new()
            .custom_flags(flags)
            .read((flags & O_RDONLY != 0) | (flags & O_RDWR != 0))
            .write((flags & O_WRONLY != 0) | (flags & O_RDWR != 0))
            .open(path)
            .map(|file| file.into())
            .map_err(|err| err.raw_os_error().unwrap())
    }
    fn close_restricted(&mut self, fd: OwnedFd) {
        _ = File::from(fd);
    }
}


fn button_hit(num: u32, idx: u32, x: f64, y: f64) -> bool {
    let button_width = DFR_WIDTH as f64 / (num + 1) as f64;
    let spacing_width = (DFR_WIDTH as f64 - num as f64 * button_width) / (num + 1) as f64;
    let left_edge = idx as f64 * (button_width + spacing_width) + spacing_width;
    if x < left_edge || x > (left_edge + button_width) {
        return false
    }
    y > 0.09 * DFR_HEIGHT as f64 && y < 0.91 * DFR_HEIGHT as f64
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

fn main() {
    let mut surface = ImageSurface::create(Format::ARgb32, DFR_HEIGHT, DFR_WIDTH).unwrap();
    let layer = FunctionLayer {
        buttons: vec![
            Button { text: "F1".to_string(), action: Key::F1 },
            Button { text: "F2".to_string(), action: Key::F2 },
            Button { text: "F3".to_string(), action: Key::F3 },
            Button { text: "F4".to_string(), action: Key::F4 },
            Button { text: "F5".to_string(), action: Key::F5 },
            Button { text: "F6".to_string(), action: Key::F6 },
            Button { text: "F7".to_string(), action: Key::F7 },
            Button { text: "F8".to_string(), action: Key::F8 },
            Button { text: "F9".to_string(), action: Key::F9 },
            Button { text: "F10".to_string(), action: Key::F10 },
            Button { text: "F11".to_string(), action: Key::F11 },
            Button { text: "F12".to_string(), action: Key::F12 }
        ]
    };
    let mut button_state = vec![false; 12];
    let mut needs_redraw = true;
    let mut drm = DrmBackend::open_card().unwrap();
    let mut input_tb = Libinput::new_with_udev(Interface);
    let mut input_main = Libinput::new_with_udev(Interface);
    input_tb.udev_assign_seat("seat-touchbar").unwrap();
    input_main.udev_assign_seat("seat0").unwrap();
    let pollfd_tb = PollFd::new(input_tb.as_raw_fd(), PollFlags::POLLIN);
    let pollfd_main = PollFd::new(input_main.as_raw_fd(), PollFlags::POLLIN);
    let mut uinput = UInputHandle::new(OpenOptions::new().write(true).open("/dev/uinput").unwrap());
    uinput.set_evbit(EventKind::Key).unwrap();
    for button in &layer.buttons {
        uinput.set_keybit(button.action).unwrap();
    }
    uinput.dev_setup(&uinput_setup {
        id: input_id {
            bustype: 0x19,
            vendor: 0x1209,
            product: 0x316E,
            version: 1
        },
        ff_effects_max: 0,
        name: [
            b'D', b'y', b'n', b'a', b'm', b'i', b'c', b' ',
            b'F', b'u', b'n', b'c', b't', b'i', b'o', b'n', b' ',
            b'R', b'o', b'w', b' ',
            b'V', b'i', b'r', b't', b'u', b'a', b'l', b' ',
            b'I', b'n', b'p', b'u', b't', b' ',
            b'D', b'e', b'v', b'i', b'c', b'e',
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0
        ]
    }).unwrap();
    uinput.dev_create().unwrap();

    let mut backlight = BacklightManager::new();

    PrivDrop::default()
        .chroot("/var/empty")
        .user("nobody")
        .group("nobody")
        .apply()
        .unwrap_or_else(|e| { panic!("Failed to drop privileges: {}", e) });

    let mut digitizer: Option<InputDevice> = None;
    let mut touches = HashMap::new();
    loop {
        if needs_redraw {
            needs_redraw = false;
            layer.draw(&surface, &button_state);
            let data = surface.data().unwrap();
            drm.map().unwrap().as_mut()[..data.len()].copy_from_slice(&data);
            drm.dirty(&[ClipRect{x1: 0, y1: 0, x2: DFR_HEIGHT as u16, y2: DFR_WIDTH as u16}]).unwrap();
        }
        poll(&mut [pollfd_tb, pollfd_main], TIMEOUT_MS).unwrap();
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
                Event::Touch(te) => {
                    if Some(te.device()) != digitizer {
                        continue
                    }
                    match te {
                        TouchEvent::Down(dn) => {
                            let x = dn.x_transformed(DFR_WIDTH as u32);
                            let y = dn.y_transformed(DFR_HEIGHT as u32);
                            let btn = (x / (DFR_WIDTH as f64 / layer.buttons.len() as f64)) as u32;
                            if button_hit(layer.buttons.len() as u32, btn, x, y) {
                                touches.insert(dn.seat_slot(), btn);
                                button_state[btn as usize] = true;
                                needs_redraw = true;
                                emit(&mut uinput, EventKind::Key, layer.buttons[btn as usize].action as u16, 1);
                                emit(&mut uinput, EventKind::Synchronize, SynchronizeKind::Report as u16, 0);
                            }
                        },
                        TouchEvent::Motion(mtn) => {
                            if !touches.contains_key(&mtn.seat_slot()) {
                                continue;
                            }

                            let x = mtn.x_transformed(DFR_WIDTH as u32);
                            let y = mtn.y_transformed(DFR_HEIGHT as u32);
                            let btn = *touches.get(&mtn.seat_slot()).unwrap();
                            let hit = button_hit(layer.buttons.len() as u32, btn, x, y);
                            if button_state[btn as usize] != hit {
                                button_state[btn as usize] = hit;
                                needs_redraw = true;
                                emit(&mut uinput, EventKind::Key, layer.buttons[btn as usize].action as u16, hit as i32);
                                emit(&mut uinput, EventKind::Synchronize, SynchronizeKind::Report as u16, 0);
                            }
                        },
                        TouchEvent::Up(up) => {
                            if !touches.contains_key(&up.seat_slot()) {
                                continue;
                            }
                            let btn = *touches.get(&up.seat_slot()).unwrap() as usize;
                            if button_state[btn] {
                                button_state[btn] = false;
                                needs_redraw = true;
                                emit(&mut uinput, EventKind::Key, layer.buttons[btn].action as u16, 0);
                                emit(&mut uinput, EventKind::Synchronize, SynchronizeKind::Report as u16, 0);
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
