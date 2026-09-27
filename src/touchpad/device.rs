use std::ffi;

use core_foundation as cf;
use eyre::ContextCompat;

use crate::touchpad::MtFunctions;

const MTDEVICE_ID_OFFSET: usize = 64;

/// Extract the device ID from the opaque `MTDevice` struct by known byte
/// offset (TODO: only for M3 `MacBook` hardware)
///
/// ## Safety
/// `dev` must point to a valid `MTDevice` object from `MTDeviceCreateList`.
pub const unsafe fn mt_device_get_id(dev: *const ffi::c_void) -> u64 {
	let mut id = 0;

	unsafe {
		std::ptr::copy_nonoverlapping(
			dev.cast::<u8>().add(MTDEVICE_ID_OFFSET),
			(&raw mut id).cast::<u8>(),
			8,
		);
	};
	id
}

/// Find the first trackpad device ID that has a working haptic actuator.
pub fn find_trackpad_device_id(mt: &MtFunctions, verbose: bool) -> eyre::Result<u64> {
	let create_list = mt
		.device_create_list
		.as_ref()
		.wrap_err("MTDeviceCreateList not available")?;
	let devices = unsafe { create_list() };
	if devices.is_null() {
		eyre::bail!("MTDeviceCreateList returned NULL");
	}
	let count = unsafe { cf::array::CFArrayGetCount(devices) };
	if verbose {
		println!("Found {count} multitouch device(s):");
	}
	let mut found: Option<u64> = None;
	for i in 0..count {
		let dev = unsafe { cf::array::CFArrayGetValueAtIndex(devices, i) };
		let dev_id = unsafe { mt_device_get_id(dev) };
		if verbose {
			println!("  [{i}] device ID: {dev_id} (0x{dev_id:x})");
		}
		let act = unsafe { (mt.actuator_create)(dev_id) };
		if !act.is_null() {
			let ret = unsafe { (mt.actuator_open)(act, 0) };
			if ret == super::K_IORETURN_SUCCESS {
				if verbose {
					println!("       ^ has haptic actuator");
				}
				unsafe { (mt.actuator_close)(act) };
				found.get_or_insert(dev_id);
			} else if verbose {
				println!("       ^ actuator open failed (0x{ret:x})");
			}
			unsafe { cf::base::CFRelease(act) };
		}
	}
	unsafe { cf::base::CFRelease(devices.cast()) };
	found.wrap_err("no haptic-capable device found")
}
