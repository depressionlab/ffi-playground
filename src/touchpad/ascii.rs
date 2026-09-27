use std::ffi;
use std::io::Write;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

use core_foundation as cf;
use eyre::ContextCompat;

use crate::touchpad::{MTTouch, MtFunctions, terminal};

const TRACKPAD_W_MM: f32 = 155.0;
const TRACKPAD_H_MM: f32 = 99.0;
const MAX_CW: usize = 140;
const MAX_CH: usize = 60;
const MAX_FINGERS: usize = 11;

pub fn run(mt: &MtFunctions) -> eyre::Result<()> {
	mt.require_listen_syms()?;
	let create_list = mt.device_create_list.unwrap();
	let devices = unsafe { create_list() };
	if devices.is_null() || unsafe { cf::array::CFArrayGetCount(devices) } == 0 {
		eyre::bail!("no multitouch devices found");
	}
	let dev = unsafe { cf::array::CFArrayGetValueAtIndex(devices, 0).cast_mut() };

	let (cw, ch) = ascii_init_dims();
	let shared = Arc::new(Mutex::new(SharedTouches {
		touches: Vec::with_capacity(MAX_FINGERS),
		n: 0,
	}));
	// Stash a raw pointer for the C callback (valid until run loop exits).
	let shared_box = Box::new(shared.clone());
	let shared_raw = Box::into_raw(shared_box);
	ASCII_SHARED.store(shared_raw as usize, Ordering::Relaxed);

	terminal::install_sigint();
	let rl = unsafe { cf::runloop::CFRunLoopGetCurrent() };
	terminal::G_RUNLOOP.store(rl as usize, Ordering::Relaxed);

	let mut term = terminal::RawTerminal::enter().wrap_err("not a tty")?;
	let fdref = terminal::stdin_source_setup();
	term.enter_alt_screen();

	unsafe { (mt.register_callback.unwrap())(dev, ascii_touch_cb) };
	terminal::mt_device_start_quiet(mt, dev);

	// Box the timer context so it has a stable address for the C callback.
	let mut ctx = Box::new(AsciiTimerContext {
		state: AsciiState::new(cw, ch),
		shared,
	});
	let ctx_ptr = std::ptr::from_mut::<AsciiTimerContext>(ctx.as_mut()).cast::<ffi::c_void>();

	let mut timer_ctx = cf::runloop::CFRunLoopTimerContext {
		version: 0,
		info: ctx_ptr,
		retain: None,
		release: None,
		copyDescription: None,
	};

	let timer = unsafe {
		cf::runloop::CFRunLoopTimerCreate(
			std::ptr::null(),
			cf::date::CFAbsoluteTimeGetCurrent(),
			1.0 / 30.0,
			0,
			0,
			ascii_timer_cb,
			&raw mut timer_ctx,
		)
	};
	unsafe { cf::runloop::CFRunLoopAddTimer(rl, timer, cf::runloop::kCFRunLoopCommonModes) };

	terminal::G_RUNNING.store(true, Ordering::Relaxed);
	while terminal::G_RUNNING.load(Ordering::Relaxed) {
		unsafe { cf::runloop::CFRunLoopRunInMode(cf::runloop::kCFRunLoopDefaultMode, 1.0, 0) };
	}

	unsafe { cf::runloop::CFRunLoopRemoveTimer(rl, timer, cf::runloop::kCFRunLoopCommonModes) };
	unsafe { cf::base::CFRelease(timer.cast()) };
	unsafe { (mt.device_stop.unwrap())(dev) };
	unsafe { cf::base::CFRelease(devices.cast()) };
	if let Some(fd) = fdref {
		unsafe { cf::base::CFRelease(fd.cast()) };
	}
	ASCII_SHARED.store(0, Ordering::Relaxed);
	drop(unsafe { Box::from_raw(shared_raw) });
	Ok(())
}

#[derive(Default)]
struct AsciiState {
	cw: usize,
	ch: usize,
	dw: usize,
	dh: usize,
	heat: Vec<f32>, // dh × dw, row-major
	pmax: f32,
	touches: Vec<MTTouch>,
	first: bool,
	ob: Vec<u8>,
}

/// Shared state between the MT callback thread and the render timer.
struct SharedTouches {
	touches: Vec<MTTouch>,
	n: usize,
}

impl AsciiState {
	fn new(cw: usize, ch: usize) -> Self {
		let dw = cw * 2;
		let dh = ch * 4;
		Self {
			cw,
			ch,
			dw,
			dh,
			heat: vec![0.0; dh * dw],
			pmax: 1.4,
			touches: Vec::new(),
			first: true,
			ob: Vec::with_capacity(1 << 18),
		}
	}

	/// Gaussian splat of touch ellipse onto heat map.
	fn paint(&mut self) {
		let mpdx = TRACKPAD_W_MM / self.dw as f32;
		let mpdy = TRACKPAD_H_MM / self.dh as f32;
		let dw = self.dw;
		let dh = self.dh;

		for t in &self.touches {
			if t.state <= 0 {
				continue;
			}
			let cx = t.norm_x * dw as f32;
			let cy = (1.0 - t.norm_y) * dh as f32;

			let sa = ((t.major_axis / 2.0) / mpdx * 1.3).max(2.0);
			let sb = ((t.minor_axis / 2.0) / mpdy * 1.3).max(2.0);
			let ca = (-t.angle).cos();
			let sn = (-t.angle).sin();

			let p = t.size;
			if p > self.pmax {
				self.pmax = p;
			}
			let pn = (p / self.pmax).max(0.15);

			let rmax = sa.max(sb) * 2.0;
			let x0 = (cx - rmax).max(0.0) as usize;
			let x1 = (cx + rmax).min(dw as f32 - 1.0) as usize;
			let y0 = (cy - rmax).max(0.0) as usize;
			let y1 = (cy + rmax).min(dh as f32 - 1.0) as usize;

			for y in y0..=y1 {
				for x in x0..=x1 {
					let dx = x as f32 - cx;
					let dy = y as f32 - cy;
					let rx = dx.mul_add(ca, -(dy * sn));
					let ry = dx.mul_add(sn, dy * ca);
					let d2 = (rx * rx) / sa.mul_add(sa, 0.01) + (ry * ry) / sb.mul_add(sb, 0.01);
					let val = pn * (-d2 * 1.5).exp();
					let cell = &mut self.heat[y * dw + x];
					if val > *cell {
						*cell = val;
					}
				}
			}
		}
	}

	fn render(&mut self) {
		self.ob.clear();

		// Braille dot bit positions: [row][col] → bit index
		const BR: [[u32; 2]; 4] = [[0, 3], [1, 4], [2, 5], [6, 7]];

		macro_rules! w {
            ($($arg:tt)*) => { write!(self.ob, $($arg)*).unwrap() };
        }

		w!("\x1b[H");
		if self.first {
			w!("\x1b[2J\x1b[?25l");
			self.first = false;
		}

		// Header
		w!(
			" \x1b[1mmactic\x1b[22m \x1b[38;5;240m│\x1b[0m {}×{}mm \
             \x1b[38;5;240m│\x1b[0m esc/q to quit\x1b[K\n",
			TRACKPAD_W_MM as i32,
			TRACKPAD_H_MM as i32,
		);

		// Top border
		w!("\x1b[38;5;240m╭");
		for _ in 0..self.cw {
			w!("─");
		}
		w!("╮\x1b[0m\n");

		// Canvas/braille heatmap
		let cw = self.cw;
		let ch = self.ch;
		let dw = self.dw;
		let dh = self.dh;

		for row in 0..ch {
			w!("\x1b[38;5;240m│\x1b[0m");
			let (mut pr, mut pg, mut pb) = (i32::MIN, i32::MIN, i32::MIN);
			for col in 0..cw {
				let dr = row * 4;
				let dc = col * 2;
				let mut cp: u32 = 0x2800;
				let mut maxp: f32 = 0.0;
				for r in 0..4usize {
					for c in 0..2usize {
						let dy = dr + r;
						let dx = dc + c;
						if dy < dh && dx < dw {
							let p = self.heat[dy * dw + dx];
							if p > 0.02 {
								cp |= 1 << BR[r][c];
								if p > maxp {
									maxp = p;
								}
							}
						}
					}
				}
				if cp == 0x2800 {
					if pr != i32::MIN {
						w!("\x1b[0m");
						(pr, pg, pb) = (i32::MIN, i32::MIN, i32::MIN);
					}
					w!(" ");
				} else {
					let (r, g, b) = heat_color(maxp);
					let (r, g, b) = (i32::from(r), i32::from(g), i32::from(b));
					if r != pr || g != pg || b != pb {
						w!("\x1b[38;2;{r};{g};{b}m");
						(pr, pg, pb) = (r, g, b);
					}
					// Encode cp as UTF-8
					let u = [
						(0xE0 | (cp >> 12)) as u8,
						(0x80 | ((cp >> 6) & 0x3F)) as u8,
						(0x80 | (cp & 0x3F)) as u8,
					];
					self.ob.extend_from_slice(&u);
				}
			}
			w!("\x1b[0m\x1b[38;5;240m│\x1b[0m\n");
		}

		// Bottom border
		w!("\x1b[38;5;240m╰");
		for _ in 0..cw {
			w!("─");
		}
		w!("╯\x1b[0m\n");

		// Status bar
		let n = self.touches.len();
		let maxpres = self.touches.iter().map(|t| t.size).fold(0.0f32, f32::max);
		let pnorm = (if self.pmax > 0.0 {
			maxpres / self.pmax
		} else {
			0.0
		})
		.min(1.0);
		let bw = 24usize;
		let filled = f32::mul_add(pnorm, bw as f32, 0.5) as usize;

		w!(" {n} finger{:<2} ", if n == 1 { " " } else { "s" });
		for i in 0..bw {
			if i < filled {
				let (r, g, b) = heat_color((i + 1) as f32 / bw as f32);
				w!("\x1b[38;2;{r};{g};{b}m█");
			} else {
				w!("\x1b[38;5;236m░");
			}
		}
		w!("\x1b[0m {maxpres:.2}");

		if n > 0 && n <= 5 {
			w!("  ");
			for t in &self.touches {
				w!(" \x1b[38;5;245m({:+.2},{:+.2})\x1b[0m", t.norm_x, t.norm_y);
			}
		}
		w!("\x1b[K\x1b[J");

		// Flush the output buffer atomically
		let mut off = 0usize;
		while off < self.ob.len() {
			let n = unsafe {
				libc::write(
					libc::STDOUT_FILENO,
					self.ob[off..].as_ptr().cast::<ffi::c_void>(),
					self.ob.len() - off,
				)
			};
			if n <= 0 {
				break;
			}
			off += n as usize;
		}
	}
}

/// Heat colormap: deep blue → cyan → green → yellow → red → white.
fn heat_color(t: f32) -> (u8, u8, u8) {
	if t <= 0.0 {
		return (0, 0, 0);
	}
	let t = t.min(1.0);
	if t < 0.12 {
		let s = t / 0.12;
		(0, 0, s.mul_add(120.0, 80.0) as u8)
	} else if t < 0.25 {
		let s = (t - 0.12) / 0.13;
		(0, (s * 180.0) as u8, s.mul_add(55.0, 200.0) as u8)
	} else if t < 0.42 {
		let s = (t - 0.25) / 0.17;
		(0, s.mul_add(75.0, 180.0) as u8, ((1.0 - s) * 255.0) as u8)
	} else if t < 0.60 {
		let s = (t - 0.42) / 0.18;
		((s * 255.0) as u8, 255, 0)
	} else if t < 0.80 {
		let s = (t - 0.60) / 0.20;
		(255, ((1.0 - s) * 255.0) as u8, 0)
	} else {
		let s = (t - 0.80) / 0.20;
		(255, (s * 255.0) as u8, (s * 255.0) as u8)
	}
}

/// Determine canvas dimensions from the terminal size.
fn ascii_init_dims() -> (usize, usize) {
	let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
	unsafe { libc::ioctl(libc::STDOUT_FILENO, libc::TIOCGWINSZ, &mut ws) };
	let tw = if ws.ws_col > 20 {
		ws.ws_col as usize
	} else {
		80
	};
	let th = if ws.ws_row > 10 {
		ws.ws_row as usize
	} else {
		40
	};

	let mut cw = (tw - 4).min(MAX_CW);
	let mut ch = ((cw as f32 * (TRACKPAD_H_MM / TRACKPAD_W_MM) / 2.0 + 0.5) as usize).min(MAX_CH);

	let max_h = if th > 4 { th - 4 } else { 4 };
	if ch > max_h {
		ch = max_h.max(4);
		cw = ((ch as f32 * 2.0).mul_add(TRACKPAD_W_MM / TRACKPAD_H_MM, 0.5) as usize).min(MAX_CW);
	}
	(cw, ch)
}

// SAFETY: the *mut c_void is a *mut Arc<Mutex<SharedTouches>> that we own for
//         the lifetime of the run loop; it is valid for the entire ascii mode
// run.
unsafe extern "C" fn ascii_touch_cb(
	_device: *mut ffi::c_void,
	touches: *mut MTTouch,
	n_fingers: ffi::c_int,
	_ts: ffi::c_double,
	_frame: ffi::c_int,
) {
	// Retrieve shared state pointer stashed in a global.
	let ptr = ASCII_SHARED.load(Ordering::Relaxed) as *mut Arc<Mutex<SharedTouches>>;
	if ptr.is_null() {
		return;
	}
	let shared = unsafe { &*ptr };
	if let Ok(mut guard) = shared.lock() {
		let n = (n_fingers as usize).min(MAX_FINGERS);
		guard.n = n;
		guard.touches.clear();
		if n > 0 {
			guard
				.touches
				.extend_from_slice(unsafe { std::slice::from_raw_parts(touches, n) });
		}
	}
}

static ASCII_SHARED: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

struct AsciiTimerContext {
	state: AsciiState,
	shared: Arc<Mutex<SharedTouches>>,
}

extern "C" fn ascii_timer_cb(_timer: cf::runloop::CFRunLoopTimerRef, info: *mut ffi::c_void) {
	let ctx = unsafe { &mut *info.cast::<AsciiTimerContext>() };

	// Snapshot shared touch data
	if let Ok(guard) = ctx.shared.lock() {
		ctx.state.touches.clear();
		ctx.state
			.touches
			.extend_from_slice(&guard.touches[..guard.n.min(guard.touches.len())]);
	}

	// Decay heat
	for cell in &mut ctx.state.heat {
		*cell *= 0.05;
	}
	// Slowly recover pmax toward baseline (1.4) after palm events
	ctx.state.pmax = 1.4f32.mul_add(0.03, ctx.state.pmax * 0.97);

	ctx.state.paint();
	ctx.state.render();
}
