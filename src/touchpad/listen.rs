use std::sync::atomic::Ordering;

use core_foundation as cf;

use crate::touchpad::{MtFunctions, device, terminal};

pub fn run(mt: &MtFunctions) -> eyre::Result<()> {
	mt.require_listen_syms()?;
	let create_list = mt.device_create_list.unwrap();
	let devices = unsafe { create_list() };
	if devices.is_null() || unsafe { cf::array::CFArrayGetCount(devices) } == 0 {
		eyre::bail!("no multitouch devices found");
	}
	let dev = unsafe { cf::array::CFArrayGetValueAtIndex(devices, 0).cast_mut() };
	let dev_id = unsafe { device::mt_device_get_id(dev) };
	println!("Listening on device {dev_id}. Touch the trackpad (esc/q/ctrl-c to stop)\n");

	terminal::install_sigint();
	let rl = unsafe { cf::runloop::CFRunLoopGetCurrent() };
	terminal::G_RUNLOOP.store(rl as usize, Ordering::Relaxed);

	let _term = terminal::RawTerminal::enter();
	let fdref = terminal::stdin_source_setup();

	unsafe { (mt.register_callback.unwrap())(dev, terminal::touch_callback) };
	terminal::mt_device_start_quiet(mt, dev);

	terminal::G_RUNNING.store(true, Ordering::Relaxed);
	while terminal::G_RUNNING.load(Ordering::Relaxed) {
		unsafe { cf::runloop::CFRunLoopRunInMode(cf::runloop::kCFRunLoopDefaultMode, 1.0, 1) };
	}

	unsafe { (mt.device_stop.unwrap())(dev) };
	unsafe { cf::base::CFRelease(devices.cast()) };
	if let Some(fd) = fdref {
		unsafe { cf::base::CFRelease(fd.cast()) };
	}
	println!("\nStopped.");
	Ok(())
}
