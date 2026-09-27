use core_foundation as cf;
use eyre::ContextCompat;

use crate::touchpad::MtFunctions;

pub fn run(mt: &MtFunctions, seq: &str, device_id: u64) -> eyre::Result<()> {
	let steps: Vec<(i32, u64)> = seq
		.split_whitespace()
		.enumerate()
		.map(|(i, tok)| {
			let (wstr, delay_ms) = if let Some(pos) = tok.find(':') {
				let delay: u64 = tok[pos + 1..]
					.parse()
					.map_err(|_| eyre::eyre!("invalid delay in step {}", i + 1))?;
				if delay > 60000 {
					eyre::bail!("delay out of range in step {}", i + 1);
				}
				(&tok[..pos], delay)
			} else {
				(tok, 200u64)
			};
			let w: i32 = wstr
				.parse()
				.ok()
				.filter(|&w: &i32| (1..=20).contains(&w))
				.wrap_err(format!("invalid waveform '{}' in step {}", wstr, i + 1))?;
			Ok((w, delay_ms))
		})
		.collect::<eyre::Result<_>>()?;

	let n = steps.len();
	for (i, (waveform, delay_ms)) in steps.iter().enumerate() {
		let act = unsafe { (mt.actuator_create)(device_id) };
		if act.is_null() {
			eyre::bail!("could not create actuator at step {}", i + 1);
		}
		let ret = unsafe { (mt.actuator_open)(act, 0) };
		if ret != super::K_IORETURN_SUCCESS {
			unsafe { cf::base::CFRelease(act) };
			eyre::bail!("MTActuatorOpen failed at step {} (0x{ret:x})", i + 1);
		}
		let ret = unsafe { (mt.actuator_actuate)(act, *waveform, 0, 0, 0) };
		unsafe { (mt.actuator_close)(act) };
		unsafe { cf::base::CFRelease(act) };
		if ret != super::K_IORETURN_SUCCESS {
			eyre::bail!(
				"MTActuatorActuate({waveform}) failed at step {} (0x{ret:x})",
				i + 1
			);
		}
		println!("  step {}: waveform {waveform}", i + 1);
		if i + 1 < n {
			unsafe { libc::usleep((*delay_ms as libc::useconds_t) * 1000) };
		}
	}
	println!("Chain complete ({n} steps)");
	Ok(())
}
