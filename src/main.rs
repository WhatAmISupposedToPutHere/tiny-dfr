use std::{
    fs::{File, OpenOptions, self},
    os::{
        fd::AsRawFd,
        unix::{io::{AsFd, BorrowedFd, OwnedFd}, fs::OpenOptionsExt}
    },
    path::{Path, PathBuf},
    collections::HashMap,
    time::Instant,
    io::Write
};
use cairo::{
    ImageSurface, Format, Context, Surface,
    FontSlant, FontWeight
};
use drm::{
    ClientCapability, Device as DrmDevice, buffer::DrmFourcc,
    control::{
        connector, Device as ControlDevice, property, ResourceHandle, atomic, AtomicCommitFlags,
        dumbbuffer::DumbBuffer, framebuffer, ClipRect
    }
};
use anyhow::{Result, anyhow};
use input::{
    Libinput, LibinputInterface, Device as InputDevice,
    event::{
        Event, device::DeviceEvent, EventTrait,
        switch::{Switch, SwitchEvent, SwitchState},
        touch::{TouchEvent, TouchEventPosition, TouchEventSlot}
    }
};
use libc::{O_RDONLY, O_RDWR, O_WRONLY};
use input_linux::{uinput::UInputHandle, EventKind, Key, SynchronizeKind};
use input_linux_sys::{uinput_setup, input_id, timeval, input_event};
use nix::poll::{poll, PollFd, PollFlags};

const DFR_WIDTH: i32 = 2008;
const DFR_HEIGHT: i32 = 64;
const BUTTON_COLOR_INACTIVE: f64 = 0.267;
const BUTTON_COLOR_ACTIVE: f64 = 0.567;
const TIMEOUT_MS: i32 = 30 * 1000;

struct Card(File);
impl AsFd for Card {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.0.as_fd()
    }
}

impl ControlDevice for Card {}
impl DrmDevice for Card {}

impl Card {
    fn open(path: &Path) -> Self {
        let mut options = OpenOptions::new();
        options.read(true);
        options.write(true);

        Card(options.open(path).unwrap())
    }
}

struct DrmBackend {
    card: Card,
    db: DumbBuffer,
    fb: framebuffer::Handle
}

impl Drop for DrmBackend {
    fn drop(&mut self) {
        self.card.destroy_framebuffer(self.fb).unwrap();
        self.card.destroy_dumb_buffer(self.db).unwrap();
    }
}

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
        c.set_source_rgb(0.0, 0.0, 0.0);
        c.paint().unwrap();
        c.select_font_face("sans-serif", FontSlant::Normal, FontWeight::Normal);
        c.set_font_size(32.0);
        for (i, button) in self.buttons.iter().enumerate() {
            let left_edge = i as f64 * (button_width + spacing_width) + spacing_width;
            let color = if active_buttons[i] { BUTTON_COLOR_ACTIVE } else { BUTTON_COLOR_INACTIVE };
            c.set_source_rgb(color, color, color);
            c.rectangle(left_edge, 0.09 * DFR_HEIGHT as f64, button_width, 0.82 * DFR_HEIGHT as f64);
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

fn find_prop_id<T: ResourceHandle>(
    card: &Card,
    handle: T,
    name: &'static str,
) -> Result<property::Handle> {
    let props = card.get_properties(handle)?;
    for id in props.as_props_and_values().0 {
        let info = card.get_property(*id)?;
        if info.name().to_str()? == name {
            return Ok(*id);
        }
    }
    return Err(anyhow!("Property not found"));
}

fn try_open_card(path: &Path) -> Result<DrmBackend> {
    let card = Card::open(path);
    card.set_client_capability(ClientCapability::UniversalPlanes, true)?;
    card.set_client_capability(ClientCapability::Atomic, true)?;
    card.acquire_master_lock()?;


    let res = card.resource_handles()?;
    let coninfo = res
        .connectors()
        .iter()
        .flat_map(|con| card.get_connector(*con, true))
        .collect::<Vec<_>>();
    let crtcinfo = res
        .crtcs()
        .iter()
        .flat_map(|crtc| card.get_crtc(*crtc))
        .collect::<Vec<_>>();

    let con = coninfo
        .iter()
        .find(|&i| i.state() == connector::State::Connected)
        .ok_or(anyhow!("No connected connectors found"))?;

    let &mode = con.modes().get(0).ok_or(anyhow!("No modes found"))?;
    let (disp_width, disp_height) = mode.size();
    if disp_height / disp_width < 30 {
        return Err(anyhow!("This does not look like a touchbar"));
    }
    let crtc = crtcinfo.get(0).ok_or(anyhow!("No crtcs found"))?;
    let fmt = DrmFourcc::Xrgb8888;
    let db = card.create_dumb_buffer((64, disp_height.into()), fmt, 32)?;

    let fb = card.add_framebuffer(&db, 24, 32)?;
    let plane = *card.plane_handles()?.get(0).ok_or(anyhow!("No planes found"))?;

    let mut atomic_req = atomic::AtomicModeReq::new();
    atomic_req.add_property(
        con.handle(),
        find_prop_id(&card, con.handle(), "CRTC_ID")?,
        property::Value::CRTC(Some(crtc.handle())),
    );
    let blob = card.create_property_blob(&mode)?;

    atomic_req.add_property(
        crtc.handle(),
        find_prop_id(&card, crtc.handle(), "MODE_ID")?,
        blob,
    );
    atomic_req.add_property(
        crtc.handle(),
        find_prop_id(&card, crtc.handle(), "ACTIVE")?,
        property::Value::Boolean(true),
    );
    atomic_req.add_property(
        plane,
        find_prop_id(&card, plane, "FB_ID")?,
        property::Value::Framebuffer(Some(fb)),
    );
    atomic_req.add_property(
        plane,
        find_prop_id(&card, plane, "CRTC_ID")?,
        property::Value::CRTC(Some(crtc.handle())),
    );
    atomic_req.add_property(
        plane,
        find_prop_id(&card, plane, "SRC_X")?,
        property::Value::UnsignedRange(0),
    );
    atomic_req.add_property(
        plane,
        find_prop_id(&card, plane, "SRC_Y")?,
        property::Value::UnsignedRange(0),
    );
    atomic_req.add_property(
        plane,
        find_prop_id(&card, plane, "SRC_W")?,
        property::Value::UnsignedRange((mode.size().0 as u64) << 16),
    );
    atomic_req.add_property(
        plane,
        find_prop_id(&card, plane, "SRC_H")?,
        property::Value::UnsignedRange((mode.size().1 as u64) << 16),
    );
    atomic_req.add_property(
        plane,
        find_prop_id(&card, plane, "CRTC_X")?,
        property::Value::SignedRange(0),
    );
    atomic_req.add_property(
        plane,
        find_prop_id(&card, plane, "CRTC_Y")?,
        property::Value::SignedRange(0),
    );
    atomic_req.add_property(
        plane,
        find_prop_id(&card, plane, "CRTC_W")?,
        property::Value::UnsignedRange(mode.size().0 as u64),
    );
    atomic_req.add_property(
        plane,
        find_prop_id(&card, plane, "CRTC_H")?,
        property::Value::UnsignedRange(mode.size().1 as u64),
    );

    card.atomic_commit(AtomicCommitFlags::ALLOW_MODESET, atomic_req)?;


    Ok(DrmBackend { card, db, fb })
}

fn open_card() -> Result<DrmBackend> {
    for entry in fs::read_dir("/dev/dri/")? {
        let entry = entry?;
        if !entry.file_name().to_string_lossy().starts_with("card") {
            continue
        }
        match try_open_card(&entry.path()) {
            Ok(card) => return Ok(card),
            Err(_) => {}
        }
    }
    Err(anyhow!("No touchbar device found"))
}

fn find_backlight() -> Result<PathBuf> {
    for entry in fs::read_dir("/sys/class/backlight/")? {
        let entry = entry?;
        if entry.file_name().to_string_lossy().contains("display-pipe") {
            let mut path = entry.path();
            path.push("brightness");
            return Ok(path);
        }
    }
    Err(anyhow!("No backlight device found"))
}

fn set_backlight(path: &Path, value: u32) {
    let mut file = OpenOptions::new().write(true).open(path).unwrap();
    file.write(format!("{}\n", value).as_bytes()).unwrap();
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
    let mut drm = open_card().unwrap();
    let bl_path = find_backlight().unwrap();
    let mut input = Libinput::new_with_udev(Interface);
    input.udev_assign_seat("seat0").unwrap();
    let pollfd = PollFd::new(input.as_raw_fd(), PollFlags::POLLIN);
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
    let mut digitizer: Option<InputDevice> = None;
    let mut touches = HashMap::new();
    let mut last_active = Instant::now();
    let mut lid_state = SwitchState::Off;
    let mut current_bl = 42;
    loop {
        if needs_redraw {
            needs_redraw = false;
            layer.draw(&surface, &button_state);
            let mut map = drm.card.map_dumb_buffer(&mut drm.db).unwrap();
            let data = surface.data().unwrap();
            map.as_mut()[..data.len()].copy_from_slice(&data);
            drm.card.dirty_framebuffer(drm.fb, &[ClipRect{x1: 0, y1: 0, x2: DFR_HEIGHT as u16, y2: DFR_WIDTH as u16}]).unwrap();
        }
        poll(&mut [pollfd], TIMEOUT_MS).unwrap();
        input.dispatch().unwrap();
        for event in &mut input {
            match event {
                Event::Device(DeviceEvent::Added(evt)) => {
                    let dev = evt.device();
                    if dev.name().contains(" Touch Bar") {
                        digitizer = Some(dev);
                    }
                },
                Event::Touch(te) => {
                    last_active = Instant::now();
                    if Some(te.device()) != digitizer {
                        continue
                    }
                    match te {
                        TouchEvent::Down(dn) => {
                            let x = dn.x_transformed(DFR_WIDTH as u32);
                            let y = dn.y_transformed(DFR_HEIGHT as u32);
                            let btn = (x / (DFR_WIDTH as f64 / 12.0)) as u32;
                            if button_hit(12, btn, x, y) {
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
                            let hit = button_hit(12, btn, x, y);
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
                Event::Keyboard(_) | Event::Pointer(_) => {
                    last_active = Instant::now();
                },
                Event::Switch(se) => {
                    match se {
                        SwitchEvent::Toggle(toggle) => {
                            match toggle.switch().unwrap() {
                                Switch::Lid => {
                                    lid_state = toggle.switch_state();
                                    println!("Lid Switch event: {}", format!("{lid_state:?}"));
                                    if toggle.switch_state() == SwitchState::Off {
                                        last_active = Instant::now();
                                    }
                                }
                                _ => {}
                            }
                        }
                        _ => {}
                    }
                }
                _ => {}
            }
        }
        let since_last_active = (Instant::now() - last_active).as_millis() as u64;
        let new_bl = if lid_state == SwitchState::On {
            0
        } else if since_last_active < TIMEOUT_MS as u64 {
            128
        } else if since_last_active < TIMEOUT_MS as u64 * 2 {
            1
        } else {
            0
        };
        if current_bl != new_bl {
            current_bl = new_bl;
            set_backlight(&bl_path, current_bl);
        }
    }
}
