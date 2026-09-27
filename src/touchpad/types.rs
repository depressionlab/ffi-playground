use std::ffi;

use cf::array::CFMutableArrayRef;
use core_foundation as cf;
use eyre::Context;

pub type IOReturn = i32;

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct MTTouch {
	pub frame: i32,
	_pad0: i32,
	pub timestamp: f64,
	pub path_index: i32,
	pub state: i32,
	pub finger_id: i32,
	pub hand_id: i32,
	pub norm_x: f32,
	pub norm_y: f32,
	pub vel_x: f32,
	pub vel_y: f32,
	pub size: f32,
	pub pressure: f32,
	pub angle: f32,
	pub major_axis: f32,
	pub minor_axis: f32,
	pub density: f32,
	pub abs_x: f32,
	pub abs_vel_x: f32,
	pub abs_vel_y: f32,
	_reserved1: i32,
	_reserved2: i32,
	pub z_pressure: f32,
}

type MTContactCallback = unsafe extern "C" fn(
	device: *mut ffi::c_void,
	touches: *mut MTTouch,
	n: ffi::c_int,
	ts: ffi::c_double,
	frame: ffi::c_int,
);

type FnMTActuatorCreateFromDeviceID = unsafe extern "C" fn(device_id: u64) -> cf::base::CFTypeRef;
type FnMTActuatorOpen =
	unsafe extern "C" fn(actuator: cf::base::CFTypeRef, options: u32) -> IOReturn;
type FnMTActuatorClose = unsafe extern "C" fn(actuator: cf::base::CFTypeRef) -> IOReturn;
type FnMTActuatorActuate = unsafe extern "C" fn(
	actuator: cf::base::CFTypeRef,
	waveform: i32,
	a1: u32,
	a2: u32,
	a3: u32,
) -> IOReturn;
type FnMTDeviceCreateList = unsafe extern "C" fn() -> CFMutableArrayRef;
type FnMTRegisterContactFrameCallback =
	unsafe extern "C" fn(device: *mut ffi::c_void, cb: MTContactCallback);
type FnMTDeviceStart = unsafe extern "C" fn(device: *mut ffi::c_void, mode: ffi::c_int);
type FnMTDeviceStop = unsafe extern "C" fn(device: *mut ffi::c_void);

/// All resolved `MultitouchSupport` function pointers.
///
/// We store raw function pointers rather than `libloading::Symbol` to avoid
/// lifetime complications. Safety is upheld by `_lib`, which keeps the dylib
/// loaded (and all symbol addresses valid) for the program's lifetime.
pub struct MtFunctions {
	pub(crate) actuator_create: FnMTActuatorCreateFromDeviceID,
	pub(crate) actuator_open: FnMTActuatorOpen,
	pub(crate) actuator_close: FnMTActuatorClose,
	pub(crate) actuator_actuate: FnMTActuatorActuate,
	pub(crate) device_create_list: Option<FnMTDeviceCreateList>,
	pub(crate) register_callback: Option<FnMTRegisterContactFrameCallback>,
	pub(crate) device_start: Option<FnMTDeviceStart>,
	pub(crate) device_stop: Option<FnMTDeviceStop>,
	/// Keeps the dynamic library loaded so all function pointers stay valid.
	_lib: libloading::Library,
}

const MT_FRAMEWORK: &str =
	"/System/Library/PrivateFrameworks/MultitouchSupport.framework/MultitouchSupport";

impl MtFunctions {
	/// Load `MultitouchSupport` via `dlopen` and resolve all symbols.
	///
	/// ## Safety
	/// We resolve each symbol immediately and transmute to a raw `fn` pointer.
	/// [`_lib`][`MtFunctions::_lib`] keeps the dylib loaded, so these pointers
	/// remain valid for the lifetime of [`MtFunctions`].
	pub(crate) fn load() -> eyre::Result<Self> {
		let lib = unsafe { libloading::Library::new(MT_FRAMEWORK) }
			.wrap_err("dlopening MultitouchSupport")?;

		macro_rules! sym {
			($name:literal, $ty:ty) => {{
				let raw: libloading::Symbol<'_, $ty> = unsafe {
					lib.get(concat!($name, "\0").as_bytes())
						.map_err(|e| eyre::eyre!("dlsym {}: {e}", $name))?
				};
				unsafe { std::mem::transmute::<$ty, $ty>(*raw) }
			}};
		}
		macro_rules! opt_sym {
			($name:literal, $ty:ty) => {{
				let raw: Option<libloading::Symbol<'_, $ty>> =
					unsafe { lib.get(concat!($name, "\0").as_bytes()).ok() };
				raw.map(|s| unsafe { std::mem::transmute::<$ty, $ty>(*s) })
			}};
		}

		Ok(Self {
			actuator_create: sym!(
				"MTActuatorCreateFromDeviceID",
				FnMTActuatorCreateFromDeviceID
			),
			actuator_open: sym!("MTActuatorOpen", FnMTActuatorOpen),
			actuator_close: sym!("MTActuatorClose", FnMTActuatorClose),
			actuator_actuate: sym!("MTActuatorActuate", FnMTActuatorActuate),
			device_create_list: opt_sym!("MTDeviceCreateList", FnMTDeviceCreateList),
			register_callback: opt_sym!(
				"MTRegisterContactFrameCallback",
				FnMTRegisterContactFrameCallback
			),
			device_start: opt_sym!("MTDeviceStart", FnMTDeviceStart),
			device_stop: opt_sym!("MTDeviceStop", FnMTDeviceStop),
			_lib: lib,
		})
	}

	pub(crate) fn require_listen_syms(&self) -> eyre::Result<()> {
		if self.device_create_list.is_none()
			|| self.register_callback.is_none()
			|| self.device_start.is_none()
			|| self.device_stop.is_none()
		{
			eyre::bail!("missing symbols for listen/ascii mode");
		}
		Ok(())
	}
}
