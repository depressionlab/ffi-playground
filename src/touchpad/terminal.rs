use std::ffi;
use std::io::Write;
use std::sync::atomic::Ordering;

use cf::filedescriptor::CFFileDescriptorRef;
use cf::runloop::CFRunLoopRef;
use core_foundation as cf;

use crate::touchpad::{MTTouch, MtFunctions};

pub static G_RUNNING: std::sync::atomic::AtomicBool =
	std::sync::atomic::AtomicBool::new(true);
pub static G_RUNLOOP: std::sync::atomic::AtomicUsize =
	std::sync::atomic::AtomicUsize::new(0);

extern "C" fn sigint_handler(_sig: ffi::c_int) {
	G_RUNNING.store(false, Ordering::Relaxed);
	let rl = G_RUNLOOP.load(Ordering::Relaxed) as CFRunLoopRef;
	if !rl.is_null() {
		unsafe { cf::runloop::CFRunLoopStop(rl) };
	}
}

pub fn install_sigint() {
	unsafe {
		let mut sa: libc::sigaction = std::mem::zeroed();
		sa.sa_sigaction = sigint_handler as *const () as usize;
		libc::sigemptyset(&raw mut sa.sa_mask);
		libc::sigaction(libc::SIGINT, &raw const sa, std::ptr::null_mut());
	}
}

pub struct RawTerminal {
	orig: libc::termios,
	alt_screen: bool,
}

impl RawTerminal {
	pub(crate) fn enter() -> Option<Self> {
		if unsafe { libc::isatty(libc::STDIN_FILENO) } == 0 {
			return None;
		}
		let mut orig: libc::termios = unsafe { std::mem::zeroed() };
		if unsafe { libc::tcgetattr(libc::STDIN_FILENO, &raw mut orig) } != 0 {
			return None;
		}
		let mut raw = orig;
		raw.c_lflag &= !(libc::ICANON | libc::ECHO);
		raw.c_cc[libc::VMIN] = 0;
		raw.c_cc[libc::VTIME] = 0;
		unsafe { libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &raw const raw) };
		Some(Self {
			orig,
			alt_screen: false,
		})
	}

	pub(crate) fn enter_alt_screen(&mut self) {
		print!("\x1b[?1049h\x1b[?25l");
		let _ = std::io::stdout().flush();
		self.alt_screen = true;
	}
}

impl Drop for RawTerminal {
	fn drop(&mut self) {
		if self.alt_screen {
			print!("\x1b[?25h\x1b[?1049l");
			let _ = std::io::stdout().flush();
		}
		unsafe { libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &raw const self.orig) };
	}
}

extern "C" fn stdin_read_cb(
	fdref: CFFileDescriptorRef,
	_flags: cf::base::CFOptionFlags,
	_info: *mut ffi::c_void,
) {
	let mut buf = [0u8; 32];
	let n = unsafe {
		libc::read(
			libc::STDIN_FILENO,
			buf.as_mut_ptr().cast::<ffi::c_void>(),
			buf.len(),
		)
	};
	for i in 0..n.max(0).cast_unsigned() {
		if buf[i] == 0x1B || buf[i] == b'q' {
			G_RUNNING.store(false, Ordering::Relaxed);
			let rl = G_RUNLOOP.load(Ordering::Relaxed) as CFRunLoopRef;
			if !rl.is_null() {
				unsafe { cf::runloop::CFRunLoopStop(rl) };
			}
			return;
		}
	}
	unsafe {
		cf::filedescriptor::CFFileDescriptorEnableCallBacks(
			fdref,
			cf::filedescriptor::kCFFileDescriptorReadCallBack,
		);
	};
}

pub fn stdin_source_setup() -> Option<CFFileDescriptorRef> {
	if unsafe { libc::isatty(libc::STDIN_FILENO) } == 0 {
		return None;
	}
	unsafe {
		let fdref = cf::filedescriptor::CFFileDescriptorCreate(
			std::ptr::null(),
			libc::STDIN_FILENO,
			0,
			stdin_read_cb,
			std::ptr::null(),
		);
		cf::filedescriptor::CFFileDescriptorEnableCallBacks(
			fdref,
			cf::filedescriptor::kCFFileDescriptorReadCallBack,
		);
		let src =
			cf::filedescriptor::CFFileDescriptorCreateRunLoopSource(std::ptr::null(), fdref, 0);
		let rl = cf::runloop::CFRunLoopGetCurrent();
		cf::runloop::CFRunLoopAddSource(rl, src, cf::runloop::kCFRunLoopCommonModes);
		cf::base::CFRelease(src.cast());
		Some(fdref)
	}
}

pub fn mt_device_start_quiet(mt: &MtFunctions, dev: *mut ffi::c_void) {
	let Some(start) = mt.device_start.as_ref() else {
		return;
	};
	unsafe {
		let _ = libc::fflush(std::ptr::null_mut());
		let so = libc::dup(libc::STDOUT_FILENO);
		let se = libc::dup(libc::STDERR_FILENO);
		let c_path = ffi::CString::new("/dev/null").unwrap();
		let nul = libc::open(c_path.as_ptr(), libc::O_WRONLY);
		if so < 0 || se < 0 || nul < 0 {
			if so >= 0 {
				libc::close(so);
			}
			if se >= 0 {
				libc::close(se);
			}
			if nul >= 0 {
				libc::close(nul);
			}
			start(dev, 0);
			return;
		}
		libc::dup2(nul, libc::STDOUT_FILENO);
		libc::dup2(nul, libc::STDERR_FILENO);
		libc::close(nul);
		start(dev, 0);
		let _ = libc::fflush(std::ptr::null_mut());
		libc::dup2(so, libc::STDOUT_FILENO);
		libc::dup2(se, libc::STDERR_FILENO);
		libc::close(so);
		libc::close(se);
	}
}

const fn state_name(state: i32) -> &'static str {
	match state {
		0 => "none",
		1 => "start",
		2 => "hover",
		3 => "make",
		4 => "touch",
		5 => "press",
		6 => "tap",
		7 => "lift",
		_ => "?",
	}
}

pub unsafe extern "C" fn touch_callback(
	_device: *mut ffi::c_void,
	touches: *mut MTTouch,
	n_fingers: ffi::c_int,
	timestamp: ffi::c_double,
	frame: ffi::c_int,
) {
	if n_fingers <= 0 {
		return;
	}
	println!("frame {frame:<6}  t={timestamp:.4}  fingers={n_fingers}");
	for i in 0..n_fingers as usize {
		let t = unsafe { &*touches.add(i) };
		println!(
			"  [{}] state={:<5}  pos=({:.3}, {:.3})  vel=({:.3}, {:.3})  \
             pressure={:.1}  size={:.3}  angle={:.2}  \
             axis=({:.2}, {:.2})  density={:.2}  \
             abs_x={:.1}mm  abs_vel=({:.1}, {:.1})mm/s  zPressure={:.3}",
			t.path_index,
			state_name(t.state),
			t.norm_x,
			t.norm_y,
			t.vel_x,
			t.vel_y,
			t.pressure,
			t.size,
			t.angle,
			t.major_axis,
			t.minor_axis,
			t.density,
			t.abs_x,
			t.abs_vel_x,
			t.abs_vel_y,
			t.z_pressure,
		);
	}
}
