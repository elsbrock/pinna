//! Hand-written mirror of the LADSPA 1.1 C header (`ladspa.h`).

use std::os::raw::{c_char, c_int, c_ulong, c_void};

pub type Data = f32;
pub type Handle = *mut c_void;

pub const PROPERTY_HARD_RT_CAPABLE: c_int = 0x4;

pub const PORT_INPUT: c_int = 0x1;
pub const PORT_OUTPUT: c_int = 0x2;
pub const PORT_CONTROL: c_int = 0x4;
pub const PORT_AUDIO: c_int = 0x8;

pub const HINT_BOUNDED_BELOW: c_int = 0x1;
pub const HINT_BOUNDED_ABOVE: c_int = 0x2;
pub const HINT_TOGGLED: c_int = 0x4;
pub const HINT_INTEGER: c_int = 0x20;
pub const HINT_DEFAULT_MASK: c_int = 0x3C0;
pub const HINT_DEFAULT_NONE: c_int = 0x0;
pub const HINT_DEFAULT_MINIMUM: c_int = 0x40;
pub const HINT_DEFAULT_LOW: c_int = 0x80;
pub const HINT_DEFAULT_MIDDLE: c_int = 0xC0;
pub const HINT_DEFAULT_HIGH: c_int = 0x100;
pub const HINT_DEFAULT_MAXIMUM: c_int = 0x140;
pub const HINT_DEFAULT_0: c_int = 0x200;
pub const HINT_DEFAULT_1: c_int = 0x240;

#[repr(C)]
pub struct PortRangeHint {
    pub hint_descriptor: c_int,
    pub lower_bound: Data,
    pub upper_bound: Data,
}

#[repr(C)]
pub struct Descriptor {
    pub unique_id: c_ulong,
    pub label: *const c_char,
    pub properties: c_int,
    pub name: *const c_char,
    pub maker: *const c_char,
    pub copyright: *const c_char,
    pub port_count: c_ulong,
    pub port_descriptors: *const c_int,
    pub port_names: *const *const c_char,
    pub port_range_hints: *const PortRangeHint,
    pub implementation_data: *mut c_void,
    pub instantiate: Option<unsafe extern "C" fn(*const Descriptor, c_ulong) -> Handle>,
    pub connect_port: Option<unsafe extern "C" fn(Handle, c_ulong, *mut Data)>,
    pub activate: Option<unsafe extern "C" fn(Handle)>,
    pub run: Option<unsafe extern "C" fn(Handle, c_ulong)>,
    pub run_adding: Option<unsafe extern "C" fn(Handle, c_ulong)>,
    pub set_run_adding_gain: Option<unsafe extern "C" fn(Handle, Data)>,
    pub deactivate: Option<unsafe extern "C" fn(Handle)>,
    pub cleanup: Option<unsafe extern "C" fn(Handle)>,
}
