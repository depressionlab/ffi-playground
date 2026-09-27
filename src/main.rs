#![allow(
	non_snake_case,
	non_camel_case_types,
	clippy::missing_safety_doc,
	unsafe_code,
	clippy::undocumented_unsafe_blocks
)]

mod cli_style;
mod smc;
mod touchpad;

use std::io::Write;

use clap::{CommandFactory, Parser};
use core_foundation::base::CFRelease;

/// Experimentations with the `MacBook`'s trackpad.
#[derive(Parser)]
#[command(version, about, long_about = None, styles = cli_style::help_styles())]
struct Cli {
	#[command(subcommand)]
	command: Command,

	/// Multitouch device ID (default: auto-detect)
	#[arg(short, long, global = true)]
	device_id: Option<u64>,
}

#[derive(clap::Subcommand)]
enum Command {
	/// Fan control playground
	FanControl {
		/// Restore all fans to automatic control, then exit
		#[arg(long, short)]
		auto: bool,

		/// Print fans and temperatures, then exit
		#[arg(long, short)]
		list: bool,
	},
	/// ASCII pressure viewer (braille heatmap)
	Ascii,
	/// Listen: stream live touch data (position, pressure, etc.)
	Listen,
	/// Actuates a waveform pattern
	Waveform {
		/// Waveform ID to actuate (default: 2)
		#[arg(long, value_enum, default_value_t = Waveforms::StrongClick)]
		id: Waveforms,

		/// Repeat count (default: 1)
		#[arg(long, short, default_value_t = 1)]
		repeat: usize,

		/// Interval between repeats in milliseconds (default: 200)
		#[arg(long, short, default_value_t = 200, value_parser = clap::value_parser!(u64).range(1..=60000))]
		interval: u64,
	},
	/// Chain: play a sequence of waveforms with delays.
	Chain {
		/// Format: 'W[:ms] W[:ms] ...' W = waveform ID, ms = delay after
		/// (default: 200)
		#[arg(long, short)]
		sequence: String,
	},
	/// List: actuate waveforms 1-20 with a pause between each.
	List,
	/// Scan: list multitouch devices and exit.
	Scan,
	/// Print a shell completion script (e.g. `ffi-playground completions fish >
	/// ~/.config/fish/completions/ffi-playground.fish`)
	#[command(hide = true)]
	Completions {
		#[arg(value_enum)]
		shell: clap_complete::Shell,
	},
}

#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, clap::ValueEnum, Debug)]
enum Waveforms {
	/// weak click
	WeakClick = 1,
	/// strong click (Force Touch Feel)
	StrongClick = 2,
	/// buzz / notification
	BuzzNotification = 3,
	/// light tap
	LightTap = 4,
	/// medium tap
	MediumTap = 5,
	/// strong tap
	StrongTap = 6,
}

//   {prog} -a              # ascii pressure heatmap
//   {prog} -f              # stream live touch data
//   {prog} -w 2            # single strong click
//   {prog} -w 3 -r 3       # buzz three times
//   {prog} -c '6:100 2:500 2'  # chain: 6, 100ms, 2, 500ms, 2
//   {prog} -l              # cycle through waveforms 1-20
//   {prog} -s              # scan for devices"

fn main() -> eyre::Result<()> {
	color_eyre::install()?;
	let cli = Cli::parse();
	let mt = touchpad::MtFunctions::load()?;

	match &cli.command {
		Command::FanControl { auto, list } => smc::run(*list, *auto),
		Command::Ascii => touchpad::ascii::run(&mt),
		Command::Listen => touchpad::listen::run(&mt),
		Command::Waveform {
			id,
			repeat,
			interval,
		} => {
			let device_id = if let Some(id) = cli.device_id {
				id
			} else {
				let id = touchpad::find_trackpad_device_id(&mt, false)?;
				println!("Using device ID: {id}\n");
				id
			};

			let waveform = *id as i32;

			// Open actuator
			let act = unsafe { (mt.actuator_create)(device_id) };
			if act.is_null() {
				eyre::bail!("could not create actuator for device {device_id}");
			}
			let ret = unsafe { (mt.actuator_open)(act, 0) };
			if ret != touchpad::K_IORETURN_SUCCESS {
				unsafe { CFRelease(act) };
				eyre::bail!("MTActuatorOpen failed (0x{ret:x})");
			}

			let mut last_ret = touchpad::K_IORETURN_SUCCESS;

			for j in 0..*repeat {
				last_ret = unsafe { (mt.actuator_actuate)(act, waveform, 0, 0, 0) };
				if last_ret != touchpad::K_IORETURN_SUCCESS {
					eprintln!("error: MTActuatorActuate({waveform}) failed (0x{last_ret:x})");
					break;
				}
				if j + 1 < *repeat {
					unsafe { (mt.actuator_close)(act) };
					unsafe { libc::usleep((*interval as libc::useconds_t) * 1000) };
					unsafe { (mt.actuator_open)(act, 0) };
				}
			}
			if last_ret == touchpad::K_IORETURN_SUCCESS {
				println!("Actuated waveform {waveform} x{repeat}");
			}

			unsafe { (mt.actuator_close)(act) };
			unsafe { CFRelease(act) };

			if last_ret != touchpad::K_IORETURN_SUCCESS {
				std::process::exit(1);
			}

			Ok(())
		}
		Command::List => {
			let device_id = if let Some(id) = cli.device_id {
				id
			} else {
				let id = touchpad::find_trackpad_device_id(&mt, false)?;
				println!("Using device ID: {id}\n");
				id
			};

			// Open actuator
			let act = unsafe { (mt.actuator_create)(device_id) };
			if act.is_null() {
				eyre::bail!("could not create actuator for device {device_id}");
			}
			let ret = unsafe { (mt.actuator_open)(act, 0) };
			if ret != touchpad::K_IORETURN_SUCCESS {
				unsafe { CFRelease(act) };
				eyre::bail!("MTActuatorOpen failed (0x{ret:x})");
			}

			let mut last_ret = touchpad::K_IORETURN_SUCCESS;

			println!("Cycling through waveforms 1-20 (500ms apart)...");
			for w in 1i32..=20 {
				print!("  waveform {w:2}: ");
				let _ = std::io::stdout().flush();
				let ret = unsafe { (mt.actuator_actuate)(act, w, 0, 0, 0) };
				if ret == touchpad::K_IORETURN_SUCCESS {
					println!("ok");
				} else {
					println!("failed (0x{ret:x})");
				}
				unsafe { (mt.actuator_close)(act) };
				unsafe { libc::usleep(500_000) };
				unsafe { (mt.actuator_open)(act, 0) };
				last_ret = ret;
			}

			unsafe { (mt.actuator_close)(act) };
			unsafe { CFRelease(act) };

			if last_ret != touchpad::K_IORETURN_SUCCESS {
				std::process::exit(1);
			}

			Ok(())
		}
		Command::Chain { sequence } => {
			let device_id = if let Some(id) = cli.device_id {
				id
			} else {
				let id = touchpad::find_trackpad_device_id(&mt, false)?;
				println!("Using device ID: {id}\n");
				id
			};

			touchpad::chain::run(&mt, sequence, device_id)
		}
		Command::Scan => {
			println!("Scanning for haptic devices...");
			touchpad::find_trackpad_device_id(&mt, true)?;
			Ok(())
		}
		Command::Completions { shell } => {
			let mut cli_command = Cli::command();
			let name = cli_command.get_name().to_string();
			clap_complete::generate(*shell, &mut cli_command, name, &mut std::io::stdout());
			Ok(())
		}
	}
}
